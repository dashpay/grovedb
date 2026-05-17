//! Insert
//! Implements functions in Element for inserting into Merk

use grovedb_costs::{
    cost_return_on_error, cost_return_on_error_default, cost_return_on_error_into,
    cost_return_on_error_into_default, cost_return_on_error_no_add, CostResult, CostsExt,
    OperationCost,
};
use grovedb_element::Element;
use grovedb_storage::StorageContext;
use grovedb_version::{check_grovedb_v0_with_cost, version::GroveVersion};

use crate::{
    element::{
        costs::ElementCostExtensions, exists::ElementExistsInStorageExtensions,
        get::ElementFetchFromStorageExtensions, tree_type::ElementTreeTypeExtensions,
    },
    BatchEntry, CryptoHash, Error, Merk, MerkOptions, Op, TreeFeatureType,
};

/// Extension trait for inserting elements into Merk storage.
pub trait ElementInsertToStorageExtensions {
    /// Insert an element in Merk under a key; path should be resolved and
    /// proper Merk should be loaded by this moment
    /// If transaction is not passed, the batch will be written immediately.
    /// If transaction is passed, the operation will be committed on the
    /// transaction commit.
    fn insert<'db, K: AsRef<[u8]>, S: StorageContext<'db>>(
        &self,
        merk: &mut Merk<S>,
        key: K,
        options: Option<MerkOptions>,
        grove_version: &GroveVersion,
    ) -> CostResult<(), Error>;

    /// Add to batch operations a "Put" op with key and serialized element.
    /// Return CostResult.
    fn insert_into_batch_operations<K: AsRef<[u8]>>(
        &self,
        key: K,
        batch_operations: &mut Vec<BatchEntry<K>>,
        feature_type: TreeFeatureType,
        grove_version: &GroveVersion,
    ) -> CostResult<(), Error>;

    /// Insert an element in Merk under a key if it doesn't yet exist; path
    /// should be resolved and proper Merk should be loaded by this moment
    /// If transaction is not passed, the batch will be written immediately.
    /// If transaction is passed, the operation will be committed on the
    /// transaction commit.
    fn insert_if_not_exists<'db, S: StorageContext<'db>>(
        &self,
        merk: &mut Merk<S>,
        key: &[u8],
        options: Option<MerkOptions>,
        grove_version: &GroveVersion,
    ) -> CostResult<bool, Error>;

    /// Adds a "Put" op to batch operations with the element and key if it
    /// doesn't exist yet. Returns CostResult.
    fn insert_if_not_exists_into_batch_operations<'db, S: StorageContext<'db>, K: AsRef<[u8]>>(
        &self,
        merk: &mut Merk<S>,
        key: K,
        batch_operations: &mut Vec<BatchEntry<K>>,
        feature_type: TreeFeatureType,
        grove_version: &GroveVersion,
    ) -> CostResult<bool, Error>;

    /// Insert an element in Merk under a key if the value is different from
    /// what already exists; path should be resolved and proper Merk should
    /// be loaded by this moment If transaction is not passed, the batch
    /// will be written immediately. If transaction is passed, the operation
    /// will be committed on the transaction commit.
    /// The bool represents if we indeed inserted.
    /// If the value changed we return the old element.
    fn insert_if_changed_value<'db, S: StorageContext<'db>>(
        &self,
        merk: &mut Merk<S>,
        key: &[u8],
        options: Option<MerkOptions>,
        grove_version: &GroveVersion,
    ) -> CostResult<(bool, Option<Element>), Error>;

    /// Adds a "Put" op to batch operations with the element and key if the
    /// value is different from what already exists; Returns CostResult.
    /// The bool represents if we indeed inserted.
    /// If the value changed we return the old element.
    fn insert_if_changed_value_into_batch_operations<'db, S: StorageContext<'db>, K: AsRef<[u8]>>(
        &self,
        merk: &mut Merk<S>,
        key: K,
        batch_operations: &mut Vec<BatchEntry<K>>,
        feature_type: TreeFeatureType,
        grove_version: &GroveVersion,
    ) -> CostResult<(bool, Option<Element>), Error>;

    /// Insert a reference element in Merk under a key; path should be resolved
    /// and proper Merk should be loaded by this moment
    /// If transaction is not passed, the batch will be written immediately.
    /// If transaction is passed, the operation will be committed on the
    /// transaction commit.
    fn insert_reference<'db, K: AsRef<[u8]>, S: StorageContext<'db>>(
        &self,
        merk: &mut Merk<S>,
        key: K,
        referenced_value: CryptoHash,
        options: Option<MerkOptions>,
        grove_version: &GroveVersion,
    ) -> CostResult<(), Error>;

    /// Adds a "Put" op to batch operations with reference and key. Returns
    /// CostResult.
    fn insert_reference_into_batch_operations<K: AsRef<[u8]>>(
        &self,
        key: K,
        referenced_value: CryptoHash,
        batch_operations: &mut Vec<BatchEntry<K>>,
        feature_type: TreeFeatureType,
        grove_version: &GroveVersion,
    ) -> CostResult<(), Error>;

    /// Insert a tree element in Merk under a key; path should be resolved
    /// and proper Merk should be loaded by this moment
    /// If transaction is not passed, the batch will be written immediately.
    /// If transaction is passed, the operation will be committed on the
    /// transaction commit.
    fn insert_subtree<'db, K: AsRef<[u8]>, S: StorageContext<'db>>(
        &self,
        merk: &mut Merk<S>,
        key: K,
        subtree_root_hash: CryptoHash,
        options: Option<MerkOptions>,
        grove_version: &GroveVersion,
    ) -> CostResult<(), Error>;

    /// Adds a "Put" op to batch operations for a subtree and key
    fn insert_subtree_into_batch_operations<K: AsRef<[u8]>>(
        &self,
        key: K,
        subtree_root_hash: CryptoHash,
        is_replace: bool,
        batch_operations: &mut Vec<BatchEntry<K>>,
        feature_type: TreeFeatureType,
        grove_version: &GroveVersion,
    ) -> CostResult<(), Error>;

    /// Insert a `CountIndexedTree` / `ProvableCountIndexedTree` element
    /// directly into Merk under a key.
    ///
    /// Carries BOTH child Merk root hashes (primary + secondary) and uses
    /// the H1-A three-input hash composition.
    fn insert_count_indexed_subtree<'db, K: AsRef<[u8]>, S: StorageContext<'db>>(
        &self,
        merk: &mut Merk<S>,
        key: K,
        primary_root_hash: CryptoHash,
        secondary_root_hash: CryptoHash,
        options: Option<MerkOptions>,
        grove_version: &GroveVersion,
    ) -> CostResult<(), Error>;

    /// Adds a "Put" op to batch operations for a `CountIndexedTree` /
    /// `ProvableCountIndexedTree` element and key.
    fn insert_count_indexed_subtree_into_batch_operations<K: AsRef<[u8]>>(
        &self,
        key: K,
        primary_root_hash: CryptoHash,
        secondary_root_hash: CryptoHash,
        is_replace: bool,
        batch_operations: &mut Vec<BatchEntry<K>>,
        feature_type: TreeFeatureType,
        grove_version: &GroveVersion,
    ) -> CostResult<(), Error>;
}

impl ElementInsertToStorageExtensions for Element {
    /// Insert an element in Merk under a key; path should be resolved and
    /// proper Merk should be loaded by this moment
    /// If transaction is not passed, the batch will be written immediately.
    /// If transaction is passed, the operation will be committed on the
    /// transaction commit.
    fn insert<'db, K: AsRef<[u8]>, S: StorageContext<'db>>(
        &self,
        merk: &mut Merk<S>,
        key: K,
        options: Option<MerkOptions>,
        grove_version: &GroveVersion,
    ) -> CostResult<(), Error> {
        check_grovedb_v0_with_cost!("insert", grove_version.grovedb_versions.element.insert);

        let serialized = cost_return_on_error_into_default!(self.serialize(grove_version));

        if self.is_non_counted() && !merk.tree_type.accepts_non_counted_children() {
            return Err(Error::InvalidInputError(
                "non-counted elements may only be inserted into non-provable count-bearing \
                 trees (CountTree or CountSumTree); Provable* count trees commit the count \
                 cryptographically and cannot host NonCounted children",
            ))
            .wrap_with_cost(Default::default());
        }

        if self.is_not_summed() && !merk.tree_type.is_sum_bearing() {
            return Err(Error::InvalidInputError(
                "not-summed elements may only be inserted into sum-bearing trees",
            ))
            .wrap_with_cost(Default::default());
        }

        if self.is_not_counted_or_summed()
            && !merk.tree_type.accepts_not_counted_or_summed_children()
        {
            return Err(Error::InvalidInputError(
                "not-counted-or-summed elements may only be inserted into CountSumTree; \
                 ProvableCountSumTree commits the count cryptographically and cannot host \
                 NotCountedOrSummed children",
            ))
            .wrap_with_cost(Default::default());
        }

        if !merk.tree_type.allows_sum_item() && self.is_sum_item() {
            return Err(Error::InvalidInputError(
                "cannot add sum item to non sum tree",
            ))
            .wrap_with_cost(Default::default());
        }

        let merk_feature_type =
            cost_return_on_error_into_default!(self.get_feature_type(merk.tree_type));
        // Use is_sum_item() (which looks through NonCounted) so that a
        // NonCounted(SumItem(..)) takes the same specialized cost path as a
        // bare SumItem(..).
        let batch_operations = if self.is_sum_item() {
            let cost = cost_return_on_error_default!(self
                .specialized_value_defined_cost(grove_version)
                .ok_or(Error::CorruptedCodeExecution(
                    "sum items should always have a value defined cost"
                )));
            [(
                key,
                Op::PutWithSpecializedCost(serialized, cost, merk_feature_type),
            )]
        } else {
            [(key, Op::Put(serialized, merk_feature_type))]
        };
        let tree_type = merk.tree_type;
        merk.apply_with_specialized_costs::<_, Vec<u8>>(
            &batch_operations,
            &[],
            options,
            &|key, value| {
                // it is possible that a normal item was being replaced with a
                Self::specialized_costs_for_key_value(
                    key,
                    value,
                    tree_type.inner_node_type(),
                    grove_version,
                )
                .map_err(|e| Error::ClientCorruptionError(e.to_string()))
            },
            Some(&Element::value_defined_cost_for_serialized_value),
            grove_version,
        )
        .map_err(|e| Error::CorruptedData(e.to_string()))
    }

    /// Add to batch operations a "Put" op with key and serialized element.
    /// Return CostResult.
    fn insert_into_batch_operations<K: AsRef<[u8]>>(
        &self,
        key: K,
        batch_operations: &mut Vec<BatchEntry<K>>,
        feature_type: TreeFeatureType,
        grove_version: &GroveVersion,
    ) -> CostResult<(), Error> {
        check_grovedb_v0_with_cost!(
            "insert_into_batch_operations",
            grove_version
                .grovedb_versions
                .element
                .insert_into_batch_operations
        );

        let serialized = match self.serialize(grove_version) {
            Ok(s) => s,
            Err(e) => return Err(e.into()).wrap_with_cost(Default::default()),
        };

        // Use is_sum_item() (which looks through NonCounted) so that a
        // NonCounted(SumItem(..)) takes the same specialized cost path as a
        // bare SumItem(..).
        let entry = if self.is_sum_item() {
            let cost = cost_return_on_error_default!(self
                .specialized_value_defined_cost(grove_version)
                .ok_or(Error::CorruptedCodeExecution(
                    "sum items should always have a value defined cost"
                )));

            (
                key,
                Op::PutWithSpecializedCost(serialized, cost, feature_type),
            )
        } else {
            (key, Op::Put(serialized, feature_type))
        };
        batch_operations.push(entry);
        Ok(()).wrap_with_cost(Default::default())
    }

    /// Insert an element in Merk under a key if it doesn't yet exist; path
    /// should be resolved and proper Merk should be loaded by this moment
    /// If transaction is not passed, the batch will be written immediately.
    /// If transaction is passed, the operation will be committed on the
    /// transaction commit.
    fn insert_if_not_exists<'db, S: StorageContext<'db>>(
        &self,
        merk: &mut Merk<S>,
        key: &[u8],
        options: Option<MerkOptions>,
        grove_version: &GroveVersion,
    ) -> CostResult<bool, Error> {
        check_grovedb_v0_with_cost!(
            "insert_if_not_exists",
            grove_version.grovedb_versions.element.insert_if_not_exists
        );

        let mut cost = OperationCost::default();
        let exists = cost_return_on_error_into!(
            &mut cost,
            self.element_at_key_already_exists(merk, key, grove_version)
        );
        if exists {
            Ok(false).wrap_with_cost(cost)
        } else {
            cost_return_on_error!(&mut cost, self.insert(merk, key, options, grove_version));
            Ok(true).wrap_with_cost(cost)
        }
    }

    /// Adds a "Put" op to batch operations with the element and key if it
    /// doesn't exist yet. Returns CostResult.
    fn insert_if_not_exists_into_batch_operations<'db, S: StorageContext<'db>, K: AsRef<[u8]>>(
        &self,
        merk: &mut Merk<S>,
        key: K,
        batch_operations: &mut Vec<BatchEntry<K>>,
        feature_type: TreeFeatureType,
        grove_version: &GroveVersion,
    ) -> CostResult<bool, Error> {
        check_grovedb_v0_with_cost!(
            "insert_if_not_exists_into_batch_operations",
            grove_version
                .grovedb_versions
                .element
                .insert_if_not_exists_into_batch_operations
        );

        let mut cost = OperationCost::default();
        let exists = cost_return_on_error_into!(
            &mut cost,
            self.element_at_key_already_exists(merk, key.as_ref(), grove_version)
        );
        if exists {
            Ok(false).wrap_with_cost(cost)
        } else {
            cost_return_on_error!(
                &mut cost,
                self.insert_into_batch_operations(
                    key,
                    batch_operations,
                    feature_type,
                    grove_version
                )
            );
            Ok(true).wrap_with_cost(cost)
        }
    }

    /// Insert an element in Merk under a key if the value is different from
    /// what already exists; path should be resolved and proper Merk should
    /// be loaded by this moment If transaction is not passed, the batch
    /// will be written immediately. If transaction is passed, the operation
    /// will be committed on the transaction commit.
    /// The bool represents if we indeed inserted.
    /// If the value changed we return the old element.
    fn insert_if_changed_value<'db, S: StorageContext<'db>>(
        &self,
        merk: &mut Merk<S>,
        key: &[u8],
        options: Option<MerkOptions>,
        grove_version: &GroveVersion,
    ) -> CostResult<(bool, Option<Element>), Error> {
        check_grovedb_v0_with_cost!(
            "insert_if_changed_value",
            grove_version
                .grovedb_versions
                .element
                .insert_if_changed_value
        );

        let mut cost = OperationCost::default();
        let previous_element = cost_return_on_error!(
            &mut cost,
            Self::get_optional_from_storage(&merk.storage, key, grove_version)
        );
        let needs_insert = match &previous_element {
            None => true,
            Some(previous_element) => previous_element != self,
        };
        if !needs_insert {
            Ok((false, None)).wrap_with_cost(cost)
        } else {
            cost_return_on_error!(&mut cost, self.insert(merk, key, options, grove_version));
            Ok((true, previous_element)).wrap_with_cost(cost)
        }
    }

    /// Adds a "Put" op to batch operations with the element and key if the
    /// value is different from what already exists; Returns CostResult.
    /// The bool represents if we indeed inserted.
    /// If the value changed we return the old element.
    fn insert_if_changed_value_into_batch_operations<
        'db,
        S: StorageContext<'db>,
        K: AsRef<[u8]>,
    >(
        &self,
        merk: &mut Merk<S>,
        key: K,
        batch_operations: &mut Vec<BatchEntry<K>>,
        feature_type: TreeFeatureType,
        grove_version: &GroveVersion,
    ) -> CostResult<(bool, Option<Element>), Error> {
        check_grovedb_v0_with_cost!(
            "insert_if_changed_value_into_batch_operations",
            grove_version
                .grovedb_versions
                .element
                .insert_if_changed_value_into_batch_operations
        );

        let mut cost = OperationCost::default();
        let previous_element = cost_return_on_error!(
            &mut cost,
            Self::get_optional_from_storage(&merk.storage, key.as_ref(), grove_version)
        );
        let needs_insert = match &previous_element {
            None => true,
            Some(previous_element) => previous_element != self,
        };
        if !needs_insert {
            Ok((false, None)).wrap_with_cost(cost)
        } else {
            cost_return_on_error!(
                &mut cost,
                self.insert_into_batch_operations(
                    key,
                    batch_operations,
                    feature_type,
                    grove_version
                )
            );
            Ok((true, previous_element)).wrap_with_cost(cost)
        }
    }

    /// Insert a reference element in Merk under a key; path should be resolved
    /// and proper Merk should be loaded by this moment
    /// If transaction is not passed, the batch will be written immediately.
    /// If transaction is passed, the operation will be committed on the
    /// transaction commit.
    fn insert_reference<'db, K: AsRef<[u8]>, S: StorageContext<'db>>(
        &self,
        merk: &mut Merk<S>,
        key: K,
        referenced_value: CryptoHash,
        options: Option<MerkOptions>,
        grove_version: &GroveVersion,
    ) -> CostResult<(), Error> {
        check_grovedb_v0_with_cost!(
            "insert_reference",
            grove_version.grovedb_versions.element.insert_reference
        );

        if self.is_non_counted() && !merk.tree_type.accepts_non_counted_children() {
            return Err(Error::InvalidInputError(
                "non-counted elements may only be inserted into non-provable count-bearing \
                 trees (CountTree or CountSumTree); Provable* count trees commit the count \
                 cryptographically and cannot host NonCounted children",
            ))
            .wrap_with_cost(Default::default());
        }

        if self.is_not_summed() && !merk.tree_type.is_sum_bearing() {
            return Err(Error::InvalidInputError(
                "not-summed elements may only be inserted into sum-bearing trees",
            ))
            .wrap_with_cost(Default::default());
        }

        if self.is_not_counted_or_summed()
            && !merk.tree_type.accepts_not_counted_or_summed_children()
        {
            return Err(Error::InvalidInputError(
                "not-counted-or-summed elements may only be inserted into CountSumTree; \
                 ProvableCountSumTree commits the count cryptographically and cannot host \
                 NotCountedOrSummed children",
            ))
            .wrap_with_cost(Default::default());
        }

        let serialized = match self.serialize(grove_version) {
            Ok(s) => s,
            Err(e) => return Err(e.into()).wrap_with_cost(Default::default()),
        };

        let mut cost = OperationCost::default();
        let merk_feature_type = cost_return_on_error!(
            &mut cost,
            self.get_feature_type(merk.tree_type)
                .wrap_with_cost(OperationCost::default())
        );

        let batch_operations = [(
            key,
            Op::PutCombinedReference(serialized, referenced_value, merk_feature_type),
        )];
        let tree_type = merk.tree_type;
        merk.apply_with_specialized_costs::<_, Vec<u8>>(
            &batch_operations,
            &[],
            options,
            &|key, value| {
                Self::specialized_costs_for_key_value(
                    key,
                    value,
                    tree_type.inner_node_type(),
                    grove_version,
                )
                .map_err(|e| Error::ClientCorruptionError(e.to_string()))
            },
            Some(&Element::value_defined_cost_for_serialized_value),
            grove_version,
        )
        .map_err(|e| Error::CorruptedData(e.to_string()))
    }

    /// Adds a "Put" op to batch operations with reference and key. Returns
    /// CostResult.
    fn insert_reference_into_batch_operations<K: AsRef<[u8]>>(
        &self,
        key: K,
        referenced_value: CryptoHash,
        batch_operations: &mut Vec<BatchEntry<K>>,
        feature_type: TreeFeatureType,
        grove_version: &GroveVersion,
    ) -> CostResult<(), Error> {
        check_grovedb_v0_with_cost!(
            "insert_reference_into_batch_operations",
            grove_version
                .grovedb_versions
                .element
                .insert_reference_into_batch_operations
        );

        let serialized = match self.serialize(grove_version) {
            Ok(s) => s,
            Err(e) => return Err(e.into()).wrap_with_cost(Default::default()),
        };

        let entry = (
            key,
            Op::PutCombinedReference(serialized, referenced_value, feature_type),
        );
        batch_operations.push(entry);
        Ok(()).wrap_with_cost(Default::default())
    }

    /// Insert a tree element in Merk under a key; path should be resolved
    /// and proper Merk should be loaded by this moment
    /// If transaction is not passed, the batch will be written immediately.
    /// If transaction is passed, the operation will be committed on the
    /// transaction commit.
    fn insert_subtree<'db, K: AsRef<[u8]>, S: StorageContext<'db>>(
        &self,
        merk: &mut Merk<S>,
        key: K,
        subtree_root_hash: CryptoHash,
        options: Option<MerkOptions>,
        grove_version: &GroveVersion,
    ) -> CostResult<(), Error> {
        check_grovedb_v0_with_cost!(
            "insert_subtree",
            grove_version.grovedb_versions.element.insert_subtree
        );

        if self.is_non_counted() && !merk.tree_type.accepts_non_counted_children() {
            return Err(Error::InvalidInputError(
                "non-counted elements may only be inserted into non-provable count-bearing \
                 trees (CountTree or CountSumTree); Provable* count trees commit the count \
                 cryptographically and cannot host NonCounted children",
            ))
            .wrap_with_cost(Default::default());
        }

        if self.is_not_summed() && !merk.tree_type.is_sum_bearing() {
            return Err(Error::InvalidInputError(
                "not-summed elements may only be inserted into sum-bearing trees",
            ))
            .wrap_with_cost(Default::default());
        }

        if self.is_not_counted_or_summed()
            && !merk.tree_type.accepts_not_counted_or_summed_children()
        {
            return Err(Error::InvalidInputError(
                "not-counted-or-summed elements may only be inserted into CountSumTree; \
                 ProvableCountSumTree commits the count cryptographically and cannot host \
                 NotCountedOrSummed children",
            ))
            .wrap_with_cost(Default::default());
        }

        let serialized = match self.serialize(grove_version) {
            Ok(s) => s,
            Err(e) => return Err(e.into()).wrap_with_cost(Default::default()),
        };

        let cost = OperationCost::default();
        let merk_feature_type =
            cost_return_on_error_no_add!(cost, self.get_feature_type(merk.tree_type));

        let cost = cost_return_on_error_no_add!(
            cost,
            self.layered_value_defined_cost(grove_version)
                .ok_or(Error::CorruptedCodeExecution(
                    "trees should always have a layered value defined cost"
                ))
        );

        let batch_operations = [(
            key,
            Op::PutLayeredReference(serialized, cost, subtree_root_hash, merk_feature_type),
        )];
        let tree_type = merk.tree_type;
        merk.apply_with_specialized_costs::<_, Vec<u8>>(
            &batch_operations,
            &[],
            options,
            &|key, value| {
                Self::specialized_costs_for_key_value(
                    key,
                    value,
                    tree_type.inner_node_type(),
                    grove_version,
                )
                .map_err(|e| Error::ClientCorruptionError(e.to_string()))
            },
            Some(&Element::value_defined_cost_for_serialized_value),
            grove_version,
        )
        .map_err(|e| Error::CorruptedData(e.to_string()))
    }

    /// Adds a "Put" op to batch operations for a subtree and key
    fn insert_subtree_into_batch_operations<K: AsRef<[u8]>>(
        &self,
        key: K,
        subtree_root_hash: CryptoHash,
        is_replace: bool,
        batch_operations: &mut Vec<BatchEntry<K>>,
        feature_type: TreeFeatureType,
        grove_version: &GroveVersion,
    ) -> CostResult<(), Error> {
        check_grovedb_v0_with_cost!(
            "insert_subtree_into_batch_operations",
            grove_version
                .grovedb_versions
                .element
                .insert_subtree_into_batch_operations
        );

        let serialized = match self.serialize(grove_version) {
            Ok(s) => s,
            Err(e) => return Err(e.into()).wrap_with_cost(Default::default()),
        };

        let cost = cost_return_on_error_default!(self
            .layered_value_defined_cost(grove_version)
            .ok_or(Error::CorruptedCodeExecution(
                "trees should always have a layered value defined cost"
            )));

        // Replacing is more efficient, but should lead to the same costs
        let entry = if is_replace {
            (
                key,
                Op::ReplaceLayeredReference(serialized, cost, subtree_root_hash, feature_type),
            )
        } else {
            (
                key,
                Op::PutLayeredReference(serialized, cost, subtree_root_hash, feature_type),
            )
        };
        batch_operations.push(entry);
        Ok(()).wrap_with_cost(Default::default())
    }

    fn insert_count_indexed_subtree<'db, K: AsRef<[u8]>, S: StorageContext<'db>>(
        &self,
        merk: &mut Merk<S>,
        key: K,
        primary_root_hash: CryptoHash,
        secondary_root_hash: CryptoHash,
        options: Option<MerkOptions>,
        grove_version: &GroveVersion,
    ) -> CostResult<(), Error> {
        check_grovedb_v0_with_cost!(
            "insert_count_indexed_subtree",
            grove_version.grovedb_versions.element.insert_subtree
        );

        if !matches!(
            self.underlying(),
            Element::CountIndexedTree(..) | Element::ProvableCountIndexedTree(..)
        ) {
            return Err(Error::InvalidInputError(
                "insert_count_indexed_subtree only accepts CountIndexedTree or \
                 ProvableCountIndexedTree elements",
            ))
            .wrap_with_cost(Default::default());
        }

        if self.is_non_counted() && !merk.tree_type.is_count_bearing() {
            return Err(Error::InvalidInputError(
                "non-counted elements may only be inserted into count-bearing trees",
            ))
            .wrap_with_cost(Default::default());
        }

        let serialized = match self.serialize(grove_version) {
            Ok(s) => s,
            Err(e) => return Err(e.into()).wrap_with_cost(Default::default()),
        };

        let cost = OperationCost::default();
        let merk_feature_type =
            cost_return_on_error_no_add!(cost, self.get_feature_type(merk.tree_type));

        let value_cost = cost_return_on_error_no_add!(
            cost,
            self.layered_value_defined_cost(grove_version)
                .ok_or(Error::CorruptedCodeExecution(
                    "count-indexed trees should always have a layered value defined cost"
                ))
        );

        let batch_operations = [(
            key,
            Op::PutLayeredCountIndexedReference(
                serialized,
                value_cost,
                primary_root_hash,
                secondary_root_hash,
                merk_feature_type,
            ),
        )];
        let tree_type = merk.tree_type;
        merk.apply_with_specialized_costs::<_, Vec<u8>>(
            &batch_operations,
            &[],
            options,
            &|key, value| {
                Self::specialized_costs_for_key_value(
                    key,
                    value,
                    tree_type.inner_node_type(),
                    grove_version,
                )
                .map_err(|e| Error::ClientCorruptionError(e.to_string()))
            },
            Some(&Element::value_defined_cost_for_serialized_value),
            grove_version,
        )
        .map_err(|e| Error::CorruptedData(format!("insert_count_indexed_subtree: {e}")))
    }

    fn insert_count_indexed_subtree_into_batch_operations<K: AsRef<[u8]>>(
        &self,
        key: K,
        primary_root_hash: CryptoHash,
        secondary_root_hash: CryptoHash,
        is_replace: bool,
        batch_operations: &mut Vec<BatchEntry<K>>,
        feature_type: TreeFeatureType,
        grove_version: &GroveVersion,
    ) -> CostResult<(), Error> {
        check_grovedb_v0_with_cost!(
            "insert_count_indexed_subtree_into_batch_operations",
            grove_version
                .grovedb_versions
                .element
                .insert_subtree_into_batch_operations
        );

        if !matches!(
            self.underlying(),
            Element::CountIndexedTree(..) | Element::ProvableCountIndexedTree(..)
        ) {
            return Err(Error::InvalidInputError(
                "insert_count_indexed_subtree_into_batch_operations only accepts \
                 CountIndexedTree or ProvableCountIndexedTree elements",
            ))
            .wrap_with_cost(Default::default());
        }

        // Mirror the non-counted destination guard from the direct
        // `insert_count_indexed_subtree` path (L696-700): a NonCounted-
        // wrapped cidx may only be inserted into a count-bearing
        // parent. The direct path checks `merk.tree_type.is_count_bearing()`;
        // the batch variant only has the per-node `feature_type` so we
        // use `feature_type.count().is_some()` as the equivalent
        // count-bearing predicate (a node is count-bearing iff its
        // feature type carries a count). Without this check, callers
        // could queue a `NonCounted(CountIndexedTree)` op with a
        // non-count-bearing feature type, bypassing the invariant the
        // direct API enforces.
        if self.is_non_counted() && feature_type.count().is_none() {
            return Err(Error::InvalidInputError(
                "non-counted elements may only be inserted into count-bearing trees",
            ))
            .wrap_with_cost(Default::default());
        }

        let serialized = match self.serialize(grove_version) {
            Ok(s) => s,
            Err(e) => return Err(e.into()).wrap_with_cost(Default::default()),
        };

        let cost = cost_return_on_error_default!(self
            .layered_value_defined_cost(grove_version)
            .ok_or(Error::CorruptedCodeExecution(
                "count-indexed trees should always have a layered value defined cost"
            )));

        let entry = if is_replace {
            (
                key,
                Op::ReplaceLayeredCountIndexedReference(
                    serialized,
                    cost,
                    primary_root_hash,
                    secondary_root_hash,
                    feature_type,
                ),
            )
        } else {
            (
                key,
                Op::PutLayeredCountIndexedReference(
                    serialized,
                    cost,
                    primary_root_hash,
                    secondary_root_hash,
                    feature_type,
                ),
            )
        };
        batch_operations.push(entry);
        Ok(()).wrap_with_cost(Default::default())
    }
}

#[cfg(all(feature = "minimal", feature = "test_utils"))]
#[cfg(test)]
mod tests {
    use grovedb_storage::{rocksdb_storage::test_utils::TempStorage, Storage, StorageBatch};

    use super::*;
    use crate::{
        element::get::ElementFetchFromStorageExtensions,
        test_utils::{empty_path_merk, empty_path_merk_read_only, TempMerk},
        TreeType,
    };

    #[test]
    fn test_success_insert() {
        let grove_version = GroveVersion::latest();
        let mut merk = TempMerk::new(grove_version);
        Element::empty_tree()
            .insert(&mut merk, b"mykey", None, grove_version)
            .unwrap()
            .expect("expected successful insertion");
        Element::new_item(b"value".to_vec())
            .insert(&mut merk, b"another-key", None, grove_version)
            .unwrap()
            .expect("expected successful insertion 2");

        assert_eq!(
            Element::get(&merk, b"another-key", true, grove_version)
                .unwrap()
                .expect("expected successful get"),
            Element::new_item(b"value".to_vec()),
        );
    }

    #[test]
    fn test_insert_if_changed_value_does_not_insert_when_value_does_not_change() {
        let grove_version = GroveVersion::latest();
        let mut merk = TempMerk::new(grove_version);

        Element::empty_tree()
            .insert(&mut merk, b"mykey", None, grove_version)
            .unwrap()
            .expect("expected successful insertion");
        Element::new_item(b"value".to_vec())
            .insert(&mut merk, b"another-key", None, grove_version)
            .unwrap()
            .expect("expected successful insertion 2");

        merk.commit(grove_version);

        let (inserted, previous) = Element::new_item(b"value".to_vec())
            .insert_if_changed_value(&mut merk, b"another-key", None, grove_version)
            .unwrap()
            .expect("expected successful insertion 2");

        merk.commit(grove_version);

        assert!(!inserted);
        assert_eq!(previous, None);
        assert_eq!(
            Element::get(&merk, b"another-key", true, grove_version)
                .unwrap()
                .expect("expected successful get"),
            Element::new_item(b"value".to_vec()),
        );
    }

    #[test]
    fn test_insert_if_changed_value_inserts_when_value_changed() {
        let grove_version = GroveVersion::latest();
        let storage = TempStorage::new();
        let batch = StorageBatch::new();
        let transaction = storage.start_transaction();
        let mut merk = empty_path_merk(&*storage, &transaction, &batch, grove_version);

        Element::empty_tree()
            .insert(&mut merk, b"mykey", None, grove_version)
            .unwrap()
            .expect("expected successful insertion");
        Element::new_item(b"value".to_vec())
            .insert(&mut merk, b"another-key", None, grove_version)
            .unwrap()
            .expect("expected successful insertion 2");

        storage
            .commit_multi_context_batch(batch, None)
            .unwrap()
            .unwrap();

        let batch = StorageBatch::new();
        let mut merk = empty_path_merk(&*storage, &transaction, &batch, grove_version);
        let (inserted, previous) = Element::new_item(b"value2".to_vec())
            .insert_if_changed_value(&mut merk, b"another-key", None, grove_version)
            .unwrap()
            .expect("expected successful insertion 2");

        assert!(inserted);
        assert_eq!(previous, Some(Element::new_item(b"value".to_vec())),);

        storage
            .commit_multi_context_batch(batch, None)
            .unwrap()
            .unwrap();
        let merk = empty_path_merk_read_only(&*storage, &transaction, grove_version);

        assert_eq!(
            Element::get(&merk, b"another-key", true, grove_version)
                .unwrap()
                .expect("expected successful get"),
            Element::new_item(b"value2".to_vec()),
        );
    }

    #[test]
    fn test_insert_if_changed_value_inserts_when_no_value() {
        let grove_version = GroveVersion::latest();
        let mut merk = TempMerk::new(grove_version);
        Element::empty_tree()
            .insert(&mut merk, b"mykey", None, grove_version)
            .unwrap()
            .expect("expected successful insertion");
        let (inserted, previous) = Element::new_item(b"value2".to_vec())
            .insert_if_changed_value(&mut merk, b"another-key", None, grove_version)
            .unwrap()
            .expect("expected successful insertion 2");

        assert!(inserted);
        assert_eq!(previous, None);

        assert_eq!(
            Element::get(&merk, b"another-key", true, grove_version)
                .unwrap()
                .expect("expected successful get"),
            Element::new_item(b"value2".to_vec()),
        );
    }

    #[test]
    fn non_counted_rejected_in_normal_tree() {
        let grove_version = GroveVersion::latest();
        let mut merk = TempMerk::new_with_tree_type(grove_version, TreeType::NormalTree);
        let nc = Element::new_non_counted(Element::new_item(b"x".to_vec())).expect("wrap ok");
        let result = nc.insert(&mut merk, b"k", None, grove_version).unwrap();
        assert!(matches!(result, Err(Error::InvalidInputError(_))));
    }

    #[test]
    fn non_counted_rejected_in_sum_tree() {
        let grove_version = GroveVersion::latest();
        let mut merk = TempMerk::new_with_tree_type(grove_version, TreeType::SumTree);
        let nc = Element::new_non_counted(Element::new_sum_item(7)).expect("wrap ok");
        let result = nc.insert(&mut merk, b"k", None, grove_version).unwrap();
        assert!(matches!(result, Err(Error::InvalidInputError(_))));
    }

    #[test]
    fn non_counted_accepted_in_count_tree_contributes_zero() {
        let grove_version = GroveVersion::latest();
        let mut merk = TempMerk::new_with_tree_type(grove_version, TreeType::CountTree);

        // Two regular items contribute 1 each; one NonCounted contributes 0.
        Element::new_item(b"a".to_vec())
            .insert(&mut merk, b"k1", None, grove_version)
            .unwrap()
            .expect("insert k1");
        Element::new_item(b"b".to_vec())
            .insert(&mut merk, b"k2", None, grove_version)
            .unwrap()
            .expect("insert k2");
        Element::new_non_counted(Element::new_item(b"c".to_vec()))
            .expect("wrap ok")
            .insert(&mut merk, b"k3", None, grove_version)
            .unwrap()
            .expect("insert k3 non-counted");

        let agg = merk.aggregate_data().expect("aggregate ok");
        assert_eq!(
            agg.as_count_u64(),
            2,
            "non-counted item should not be counted"
        );
    }

    #[test]
    fn non_counted_rejected_in_provable_count_sum_tree() {
        // ProvableCountSumTree commits its aggregate count into every node
        // hash, so a NonCounted child would commit a cryptographic count
        // that diverges from the actual element count. The merk-layer
        // insert guard rejects the wrapper. Symmetric coverage for
        // ProvableCountTree lives in
        // `non_counted_rejected_in_provable_count_tree`.
        let grove_version = GroveVersion::latest();
        let mut merk = TempMerk::new_with_tree_type(grove_version, TreeType::ProvableCountSumTree);
        let nc = Element::new_non_counted(Element::new_sum_item(10)).expect("wrap ok");
        let result = nc.insert(&mut merk, b"k", None, grove_version).unwrap();
        assert!(matches!(result, Err(Error::InvalidInputError(_))));
    }

    #[test]
    fn non_counted_rejected_in_provable_count_tree() {
        // Same rejection as above, against a `ProvableCountTree` parent.
        let grove_version = GroveVersion::latest();
        let mut merk = TempMerk::new_with_tree_type(grove_version, TreeType::ProvableCountTree);
        let nc = Element::new_non_counted(Element::new_item(b"x".to_vec())).expect("wrap ok");
        let result = nc.insert(&mut merk, b"k", None, grove_version).unwrap();
        assert!(matches!(result, Err(Error::InvalidInputError(_))));
    }

    #[test]
    fn non_counted_subtree_rejected_in_provable_count_tree() {
        // Same rejection via the subtree entry point (used for tree
        // children). This is the entry point a NonCounted-wrapped tree
        // would actually hit when inserted under a ProvableCountTree.
        let grove_version = GroveVersion::latest();
        let mut merk = TempMerk::new_with_tree_type(grove_version, TreeType::ProvableCountTree);
        let nc_tree = Element::new_non_counted(Element::empty_count_tree()).expect("wrap ok");
        let result = nc_tree
            .insert_subtree(&mut merk, b"k", [0u8; 32], None, grove_version)
            .unwrap();
        assert!(matches!(result, Err(Error::InvalidInputError(_))));
    }

    #[test]
    fn non_counted_subtree_rejected_via_insert_subtree_in_normal_tree() {
        // Wrap a tree element and try inserting it into a non-count-bearing
        // tree via the subtree path (used by GroveDB for tree elements).
        let grove_version = GroveVersion::latest();
        let mut merk = TempMerk::new_with_tree_type(grove_version, TreeType::NormalTree);
        let nc_tree = Element::new_non_counted(Element::empty_tree()).expect("wrap ok");
        let result = nc_tree
            .insert_subtree(&mut merk, b"k", [0u8; 32], None, grove_version)
            .unwrap();
        assert!(matches!(result, Err(Error::InvalidInputError(_))));
    }

    #[test]
    fn not_summed_rejected_in_normal_tree() {
        let grove_version = GroveVersion::latest();
        let mut merk = TempMerk::new_with_tree_type(grove_version, TreeType::NormalTree);
        let ns = Element::new_not_summed(Element::new_sum_tree(None)).expect("wrap ok");
        let result = ns.insert(&mut merk, b"k", None, grove_version).unwrap();
        assert!(matches!(result, Err(Error::InvalidInputError(_))));
    }

    #[test]
    fn not_summed_rejected_in_count_tree() {
        let grove_version = GroveVersion::latest();
        let mut merk = TempMerk::new_with_tree_type(grove_version, TreeType::CountTree);
        let ns = Element::new_not_summed(Element::new_sum_tree(None)).expect("wrap ok");
        let result = ns
            .insert_subtree(&mut merk, b"k", [0u8; 32], None, grove_version)
            .unwrap();
        assert!(matches!(result, Err(Error::InvalidInputError(_))));
    }

    #[test]
    fn not_summed_constructor_rejects_non_sum_tree_inner() {
        // Items, references, plain trees, and non-sum-tree variants must all
        // be rejected at construction time.
        assert!(Element::new_not_summed(Element::new_item(b"x".to_vec())).is_err());
        assert!(Element::new_not_summed(Element::new_sum_item(7)).is_err());
        assert!(Element::new_not_summed(Element::new_tree(None)).is_err());
        assert!(Element::new_not_summed(Element::new_count_tree(None)).is_err());
        assert!(Element::new_not_summed(Element::new_provable_count_tree(None)).is_err());
        // Wrappers cannot nest.
        let nc = Element::new_non_counted(Element::new_sum_tree(None)).expect("wrap ok");
        assert!(Element::new_not_summed(nc).is_err());
        let ns = Element::new_not_summed(Element::new_sum_tree(None)).expect("wrap ok");
        assert!(Element::new_not_summed(ns).is_err());
        // The four sum-tree variants are accepted.
        assert!(Element::new_not_summed(Element::new_sum_tree(None)).is_ok());
        assert!(Element::new_not_summed(Element::new_big_sum_tree(None)).is_ok());
        assert!(Element::new_not_summed(Element::new_count_sum_tree(None)).is_ok());
        assert!(Element::new_not_summed(Element::new_provable_count_sum_tree(None)).is_ok());
    }

    #[test]
    fn not_summed_accepted_in_sum_tree_contributes_zero_sum() {
        let grove_version = GroveVersion::latest();
        let mut merk = TempMerk::new_with_tree_type(grove_version, TreeType::SumTree);

        // One bare sum item contributes 7.
        Element::new_sum_item(7)
            .insert(&mut merk, b"k1", None, grove_version)
            .unwrap()
            .expect("insert k1");
        // A bare SumTree(_, 100, _) child via insert_subtree would
        // contribute 100. The wrapped version must contribute 0.
        let ns_subtree = Element::new_not_summed(Element::new_sum_tree_with_flags_and_sum_value(
            None, 100, None,
        ))
        .expect("wrap ok");
        ns_subtree
            .insert_subtree(&mut merk, b"k2", [0u8; 32], None, grove_version)
            .unwrap()
            .expect("insert wrapped sum tree subtree");

        let agg = merk.aggregate_data().expect("aggregate ok");
        assert_eq!(
            agg.as_sum_i64(),
            7,
            "wrapped sum tree's 100 should be suppressed; only the bare sum item contributes"
        );
    }

    #[test]
    fn not_counted_or_summed_rejected_in_normal_tree() {
        let grove_version = GroveVersion::latest();
        let mut merk = TempMerk::new_with_tree_type(grove_version, TreeType::NormalTree);
        let w = Element::new_not_counted_or_summed(Element::new_sum_tree(None)).expect("wrap ok");
        let result = w
            .insert_subtree(&mut merk, b"k", [0u8; 32], None, grove_version)
            .unwrap();
        assert!(matches!(result, Err(Error::InvalidInputError(_))));
    }

    #[test]
    fn not_counted_or_summed_rejected_in_sum_tree() {
        // SumTree bears only a sum, not a count, so suppressing both axes
        // has no meaning. The guard must reject.
        let grove_version = GroveVersion::latest();
        let mut merk = TempMerk::new_with_tree_type(grove_version, TreeType::SumTree);
        let w = Element::new_not_counted_or_summed(Element::new_sum_tree(None)).expect("wrap ok");
        let result = w
            .insert_subtree(&mut merk, b"k", [0u8; 32], None, grove_version)
            .unwrap();
        assert!(matches!(result, Err(Error::InvalidInputError(_))));
    }

    #[test]
    fn not_counted_or_summed_rejected_in_count_tree() {
        // CountTree bears only a count, not a sum, so suppressing both axes
        // has no meaning. The guard must reject.
        let grove_version = GroveVersion::latest();
        let mut merk = TempMerk::new_with_tree_type(grove_version, TreeType::CountTree);
        let w = Element::new_not_counted_or_summed(Element::new_sum_tree(None)).expect("wrap ok");
        let result = w
            .insert_subtree(&mut merk, b"k", [0u8; 32], None, grove_version)
            .unwrap();
        assert!(matches!(result, Err(Error::InvalidInputError(_))));
    }

    #[test]
    fn not_counted_or_summed_round_trips_through_storage() {
        // Insert a wrapped tree, then read it back via `Element::get`. This
        // exercises the v1 storage-read path (and the wrapper-overhead
        // accounting in `merk/src/element/get.rs`).
        let grove_version = GroveVersion::latest();
        let mut merk = TempMerk::new_with_tree_type(grove_version, TreeType::CountSumTree);

        let inner = Element::new_count_sum_tree_with_flags_and_sum_and_count_value(
            None,
            3,
            42,
            Some(vec![9, 9, 9]),
        );
        let wrapped = Element::new_not_counted_or_summed(inner.clone()).expect("wrap ok");

        wrapped
            .clone()
            .insert_subtree(&mut merk, b"k", [0u8; 32], None, grove_version)
            .unwrap()
            .expect("insert");

        let read_back = Element::get(&merk, b"k", true, grove_version)
            .unwrap()
            .expect("get");
        assert_eq!(read_back, wrapped);
        // Inner structure preserved.
        if let Element::NotCountedOrSummed(boxed) = read_back {
            assert!(
                matches!(*boxed, Element::CountSumTree(_, 3, 42, ref f) if f == &Some(vec![9, 9, 9]))
            );
        } else {
            panic!("expected NotCountedOrSummed");
        }
    }

    #[test]
    fn not_counted_or_summed_rejected_via_insert_entry_point() {
        // The wrapper's guard fires in the `insert` entry point too, not
        // just `insert_subtree`. Use the bare `insert` API on a wrong
        // parent to exercise that guard. (`new_not_counted_or_summed`
        // only allows tree inners, so this is an unusual API use — but
        // the guard is symmetric across all three entry points.)
        let grove_version = GroveVersion::latest();
        let mut merk = TempMerk::new_with_tree_type(grove_version, TreeType::NormalTree);
        let w = Element::new_not_counted_or_summed(Element::new_sum_tree(None)).expect("wrap ok");
        let result = w.insert(&mut merk, b"k", None, grove_version).unwrap();
        assert!(matches!(result, Err(Error::InvalidInputError(_))));
    }

    #[test]
    fn not_counted_or_summed_rejected_via_insert_reference_entry_point() {
        // Symmetric coverage for the `insert_reference` guard.
        let grove_version = GroveVersion::latest();
        let mut merk = TempMerk::new_with_tree_type(grove_version, TreeType::NormalTree);
        let w = Element::new_not_counted_or_summed(Element::new_sum_tree(None)).expect("wrap ok");
        let result = w
            .insert_reference(&mut merk, b"k", [0u8; 32], None, grove_version)
            .unwrap();
        assert!(matches!(result, Err(Error::InvalidInputError(_))));
    }

    #[test]
    fn not_counted_or_summed_rejected_in_provable_count_sum_tree() {
        // `ProvableCountSumTree` commits its count into every node hash,
        // so a NotCountedOrSummed child would commit a cryptographic
        // count that diverges from the actual element count. The only
        // accepted parent for this wrapper is the non-provable
        // `CountSumTree` (see
        // `not_counted_or_summed_in_count_sum_tree_excludes_both_axes`).
        let grove_version = GroveVersion::latest();
        let mut merk = TempMerk::new_with_tree_type(grove_version, TreeType::ProvableCountSumTree);
        let w = Element::new_not_counted_or_summed(
            Element::new_provable_count_sum_tree_with_flags_and_sum_and_count_value(
                None, 3, 100, None,
            ),
        )
        .expect("wrap ok");
        let result = w
            .insert_subtree(&mut merk, b"k", [0u8; 32], None, grove_version)
            .unwrap();
        assert!(matches!(result, Err(Error::InvalidInputError(_))));
    }

    #[test]
    fn not_counted_or_summed_in_count_sum_tree_excludes_both_axes() {
        // A NotCountedOrSummed(SumTree(_, 100)) inside a CountSumTree must
        // contribute (count=0, sum=0). One bare sum item contributes (1, 7).
        let grove_version = GroveVersion::latest();
        let mut merk = TempMerk::new_with_tree_type(grove_version, TreeType::CountSumTree);

        Element::new_sum_item(7)
            .insert(&mut merk, b"k1", None, grove_version)
            .unwrap()
            .expect("insert sum item");

        let w = Element::new_not_counted_or_summed(Element::new_sum_tree_with_flags_and_sum_value(
            None, 100, None,
        ))
        .expect("wrap ok");
        w.insert_subtree(&mut merk, b"k2", [0u8; 32], None, grove_version)
            .unwrap()
            .expect("insert wrapped sum tree");

        let agg = merk.aggregate_data().expect("aggregate ok");
        assert_eq!(
            agg.as_count_u64(),
            1,
            "wrapped sum tree must not be counted; got {:?}",
            agg
        );
        assert_eq!(
            agg.as_sum_i64(),
            7,
            "wrapped sum tree's 100 must not propagate; got {:?}",
            agg
        );
    }

    #[test]
    fn not_summed_in_provable_count_sum_tree_keeps_count_drops_sum() {
        // A NotSummed(SumTree(_, 100, _)) inside a ProvableCountSumTree
        // contributes count = 1, sum = 0.
        let grove_version = GroveVersion::latest();
        let mut merk = TempMerk::new_with_tree_type(grove_version, TreeType::ProvableCountSumTree);

        let ns = Element::new_not_summed(Element::new_sum_tree_with_flags_and_sum_value(
            None, 100, None,
        ))
        .expect("wrap ok");
        ns.insert_subtree(&mut merk, b"k", [0u8; 32], None, grove_version)
            .unwrap()
            .expect("insert wrapped sum tree");

        let agg = merk.aggregate_data().expect("aggregate ok");
        assert_eq!(agg.as_count_u64(), 1);
        assert_eq!(agg.as_sum_i64(), 0);
    }

    #[test]
    fn non_counted_count_tree_inside_count_tree_suppresses_subtree_count() {
        // Bare ProvableCountTree(_, 5, _) inside CountTree contributes 5.
        // Wrapped as NonCounted, contributes 0.
        let grove_version = GroveVersion::latest();
        let mut merk = TempMerk::new_with_tree_type(grove_version, TreeType::CountTree);

        Element::new_item(b"a".to_vec())
            .insert(&mut merk, b"k1", None, grove_version)
            .unwrap()
            .expect("insert k1");
        let pct = Element::new_provable_count_tree_with_flags_and_count_value(None, 5, None);
        Element::new_non_counted(pct)
            .expect("wrap ok")
            .insert(&mut merk, b"k_nc", None, grove_version)
            .unwrap()
            .expect("insert nc pct");

        let agg = merk.aggregate_data().expect("aggregate ok");
        assert_eq!(
            agg.as_count_u64(),
            1,
            "nested count tree's 5 should be suppressed; only the bare item should count"
        );
    }

    // =====================================================================
    // Coverage for cidx-specific subtree insert error branches
    // (merk/src/element/insert.rs:685-700, 768-777).
    // =====================================================================

    #[test]
    fn insert_count_indexed_subtree_rejects_non_cidx_element() {
        // Coverage for L685-694 — insert_count_indexed_subtree must
        // reject any element whose underlying type is not
        // CountIndexedTree / ProvableCountIndexedTree.
        use crate::tree::hash::NULL_HASH;

        let grove_version = GroveVersion::latest();
        let mut merk = TempMerk::new_with_tree_type(grove_version, TreeType::NormalTree);

        // A plain item is not a cidx element.
        let plain_item = Element::new_item(b"v".to_vec());
        let result = plain_item
            .insert_count_indexed_subtree(
                &mut merk,
                b"k",
                NULL_HASH,
                NULL_HASH,
                None,
                grove_version,
            )
            .unwrap();
        match result {
            Err(Error::InvalidInputError(msg)) => {
                assert!(
                    msg.contains("only accepts CountIndexedTree"),
                    "expected non-cidx error, got: {msg}"
                );
            }
            other => panic!(
                "expected InvalidInputError(non-cidx element), got: {:?}",
                other
            ),
        }

        // A regular tree element is also not a cidx element.
        let plain_tree = Element::empty_tree();
        let result = plain_tree
            .insert_count_indexed_subtree(
                &mut merk,
                b"k2",
                NULL_HASH,
                NULL_HASH,
                None,
                grove_version,
            )
            .unwrap();
        assert!(
            matches!(result, Err(Error::InvalidInputError(_))),
            "regular tree must be rejected, got: {:?}",
            result
        );
    }

    #[test]
    fn insert_count_indexed_subtree_rejects_non_counted_wrapper_into_normal_tree() {
        // Coverage for L696-700 — non-counted wrapper elements may
        // only be inserted into count-bearing trees. A NonCounted-
        // wrapped cidx into a NormalTree merk must be rejected.
        use crate::tree::hash::NULL_HASH;

        let grove_version = GroveVersion::latest();
        let mut merk = TempMerk::new_with_tree_type(grove_version, TreeType::NormalTree);

        // Wrap a cidx in NonCounted, then attempt to insert into a
        // NormalTree (not count-bearing).
        let cidx_inner = Element::empty_count_indexed_tree();
        let wrapped = Element::new_non_counted(cidx_inner).expect("wrap");
        let result = wrapped
            .insert_count_indexed_subtree(
                &mut merk,
                b"k",
                NULL_HASH,
                NULL_HASH,
                None,
                grove_version,
            )
            .unwrap();
        match result {
            Err(Error::InvalidInputError(msg)) => {
                assert!(
                    msg.contains(
                        "non-counted elements may only be inserted into \
                                  count-bearing trees"
                    ),
                    "expected non-counted-wrong-tree error, got: {msg}"
                );
            }
            other => panic!(
                "expected InvalidInputError(non-counted wrapper into non-count-bearing tree), \
                 got: {:?}",
                other
            ),
        }
    }

    #[test]
    fn insert_count_indexed_subtree_into_batch_operations_rejects_non_cidx_element() {
        // Coverage for L768-777 — the batch-operations variant of
        // insert_count_indexed_subtree has the same non-cidx reject.
        use crate::tree::hash::NULL_HASH;
        use crate::TreeFeatureType::BasicMerkNode;

        let grove_version = GroveVersion::latest();
        let mut batch_ops: Vec<BatchEntry<&[u8]>> = Vec::new();

        // Plain item routed through the batch-ops API.
        let plain_item = Element::new_item(b"v".to_vec());
        let key: &[u8] = b"k";
        let result = plain_item
            .insert_count_indexed_subtree_into_batch_operations(
                key,
                NULL_HASH,
                NULL_HASH,
                false,
                &mut batch_ops,
                BasicMerkNode,
                grove_version,
            )
            .unwrap();
        match result {
            Err(Error::InvalidInputError(msg)) => {
                assert!(
                    msg.contains("only accepts CountIndexedTree"),
                    "expected non-cidx error, got: {msg}"
                );
            }
            other => panic!(
                "expected InvalidInputError(non-cidx element in batch), got: {:?}",
                other
            ),
        }
        assert!(
            batch_ops.is_empty(),
            "rejected op must not push to batch operations vec"
        );
    }

    #[test]
    fn insert_count_indexed_subtree_into_batch_operations_rejects_non_counted_wrapper_with_non_count_bearing_feature_type(
    ) {
        // Coverage for the new non-counted-destination guard (CodeRabbit
        // finding on 2026-05-11 review). The direct
        // insert_count_indexed_subtree path already enforces:
        //   `NonCounted(...) + non-count-bearing merk → InvalidInput`
        // The batch variant must enforce the equivalent invariant
        // using its only available signal (`feature_type`): a node is
        // count-bearing iff `feature_type.count().is_some()`. Without
        // this check, callers could queue a NonCounted(cidx) op with
        // BasicMerkNode and bypass the guard.
        use crate::tree::hash::NULL_HASH;
        use crate::TreeFeatureType::BasicMerkNode;

        let grove_version = GroveVersion::latest();
        let mut batch_ops: Vec<BatchEntry<&[u8]>> = Vec::new();

        // NonCounted-wrapped cidx + BasicMerkNode (not count-bearing).
        let cidx_inner = Element::empty_count_indexed_tree();
        let wrapped = Element::new_non_counted(cidx_inner).expect("wrap");
        let key: &[u8] = b"k";
        let result = wrapped
            .insert_count_indexed_subtree_into_batch_operations(
                key,
                NULL_HASH,
                NULL_HASH,
                false,
                &mut batch_ops,
                BasicMerkNode, // NOT count-bearing
                grove_version,
            )
            .unwrap();
        match result {
            Err(Error::InvalidInputError(msg)) => {
                assert!(
                    msg.contains(
                        "non-counted elements may only be inserted into \
                                  count-bearing trees"
                    ),
                    "expected non-counted-wrong-destination error, got: {msg}"
                );
            }
            other => panic!(
                "expected InvalidInputError(non-counted into non-count-bearing), \
                 got: {:?}",
                other
            ),
        }
        assert!(
            batch_ops.is_empty(),
            "rejected op must not push to batch_operations vec"
        );

        // Sanity: same wrapped element with a count-bearing feature
        // type SUCCEEDS (the guard only fires on non-count-bearing).
        let result_ok = wrapped
            .insert_count_indexed_subtree_into_batch_operations(
                key,
                NULL_HASH,
                NULL_HASH,
                false,
                &mut batch_ops,
                crate::TreeFeatureType::CountedMerkNode(0),
                grove_version,
            )
            .unwrap();
        assert!(
            result_ok.is_ok(),
            "non-counted cidx into count-bearing tree must be accepted, got: {:?}",
            result_ok
        );
    }

    #[test]
    fn insert_count_indexed_subtree_into_batch_operations_replace_variant_for_cidx() {
        // Coverage for L790-799 — `is_replace = true` branch using
        // Op::ReplaceLayeredCountIndexedReference (vs the Put variant
        // at L802-811 which is `is_replace = false`).
        use crate::tree::hash::NULL_HASH;
        use crate::TreeFeatureType;

        let grove_version = GroveVersion::latest();
        let mut batch_ops: Vec<BatchEntry<&[u8]>> = Vec::new();

        let cidx = Element::empty_count_indexed_tree();
        let key: &[u8] = b"k";
        cidx.insert_count_indexed_subtree_into_batch_operations(
            key,
            NULL_HASH,
            NULL_HASH,
            true, // is_replace
            &mut batch_ops,
            TreeFeatureType::CountedMerkNode(0),
            grove_version,
        )
        .unwrap()
        .expect("replace variant must succeed for a cidx element");
        assert_eq!(batch_ops.len(), 1, "one op must be queued");
        // The pushed op should be Op::ReplaceLayeredCountIndexedReference.
        match &batch_ops[0].1 {
            Op::ReplaceLayeredCountIndexedReference(..) => {}
            other => panic!("expected ReplaceLayered, got: {:?}", other),
        }
    }
}
