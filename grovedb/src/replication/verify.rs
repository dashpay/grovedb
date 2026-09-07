//! Bind restored element bytes to the value hashes authenticated by Merk chunks.
//!
//! References can point into subtrees that arrive later, so their bindings
//! must be checked after discovery completes and before the final commit.

use std::collections::HashSet;

use grovedb_merk::{
    element::{costs::ElementCostExtensions, ElementExt},
    tree::{combine_hash, kv::ValueDefinedCostType, value_hash, TreeNode},
    Merk,
};
use grovedb_storage::{rocksdb_storage::RocksDbStorage, RawIterator, Storage, StorageContext};
use grovedb_version::version::GroveVersion;

use crate::{
    operations::{
        get::MAX_REFERENCE_HOPS,
        indexed_tree::{axis_secondary_tree_type, decode_axis_row_reference, indexed_element_axes},
    },
    reference_path::{path_from_reference_path_type, path_from_reference_qualified_path_type},
    Element, Error, GroveDb, Transaction,
};

/// Visit one stored node at a time without loading the whole Merk into its
/// cache or retaining a collection of reference rows.
fn visit_nodes<'db, S: StorageContext<'db>>(
    merk: &Merk<S>,
    grove_version: &GroveVersion,
    mut visit: impl FnMut(&TreeNode) -> Result<(), Error>,
) -> Result<(), Error> {
    let mut iter = merk.storage.raw_iter();
    iter.seek_to_first().unwrap();
    while iter.valid().unwrap() {
        let key = iter.key().unwrap().ok_or_else(|| {
            Error::CorruptedData("missing restored node key during verification".to_string())
        })?;
        let bytes = iter.value().unwrap().ok_or_else(|| {
            Error::CorruptedData("missing restored node during verification".to_string())
        })?;
        let node = TreeNode::decode(
            key.to_vec(),
            bytes,
            None::<&fn(&[u8], &GroveVersion) -> Option<ValueDefinedCostType>>,
            grove_version,
        )
        .map_err(|e| Error::CorruptedData(format!("cannot decode restored node: {e}")))?;
        visit(&node)?;
        iter.next().unwrap();
    }
    Ok(())
}

impl GroveDb {
    /// Resolve without the query API's terminal-wrapper removal. Batch
    /// references authenticate the stored terminal, including its wrapper.
    fn restored_reference_target(
        &self,
        mut path: Vec<Vec<u8>>,
        transaction: &Transaction,
        grove_version: &GroveVersion,
    ) -> Result<Element, Error> {
        let mut visited = HashSet::new();
        for _ in 0..MAX_REFERENCE_HOPS {
            if !visited.insert(path.clone()) {
                return Err(Error::CyclicReference);
            }
            let (key, parent) = path
                .split_last()
                .ok_or(Error::CorruptedPath("empty reference path".to_string()))?;
            let element = self
                .get_raw_caching_optional(
                    parent.into(),
                    key,
                    false,
                    Some(transaction),
                    grove_version,
                )
                .value?;
            match element.underlying() {
                Element::Reference(reference_path, ..)
                | Element::ReferenceWithSumItem(reference_path, ..) => {
                    path = path_from_reference_qualified_path_type(reference_path.clone(), &path)?;
                }
                // A bidirectional edge is followed like any other hop; the
                // chain's members commit to the terminal's LOGICAL hash.
                Element::BidirectionalReference(reference, _) => {
                    path = path_from_reference_qualified_path_type(
                        reference.forward_reference_path.clone(),
                        &path,
                    )?;
                }
                _ => return Ok(element),
            }
        }
        Err(Error::ReferenceLimit)
    }

    pub(super) fn verify_restored_value_hashes(
        &self,
        transaction: &Transaction,
        grove_version: &GroveVersion,
    ) -> Result<(), Error> {
        // Only the subtree frontier is retained. Neither item count nor
        // reference count determines the memory used by this pass.
        let mut pending = vec![(Vec::<Vec<u8>>::new(), None::<Element>)];
        while let Some((path, indexed_element)) = pending.pop() {
            let merk = self
                .open_transactional_merk_at_path(
                    path.as_slice().into(),
                    transaction,
                    None,
                    grove_version,
                )
                .value?;

            visit_nodes(&merk, grove_version, |node| {
                let bytes = node.value_as_slice();
                let element = Element::deserialize(bytes, grove_version)?;
                let actual_value_hash = value_hash(bytes).unwrap();
                let expected_value_hash = match element.underlying() {
                    Element::Reference(reference_path, ..)
                    | Element::ReferenceWithSumItem(reference_path, ..) => {
                        let target_path = path_from_reference_path_type(
                            reference_path.clone(),
                            &path,
                            Some(node.key()),
                        )?;
                        let target = self.restored_reference_target(
                            target_path,
                            transaction,
                            grove_version,
                        )?;
                        let target_hash = value_hash(&target.serialize(grove_version)?).unwrap();
                        let combined = combine_hash(&actual_value_hash, &target_hash).unwrap();
                        if combined != *node.value_hash()
                            && matches!(
                                target,
                                Element::NonCounted(_)
                                    | Element::NotSummed(_)
                                    | Element::NotCountedOrSummed(_)
                            )
                        {
                            // Direct inserts use follow_reference, which strips
                            // the terminal wrapper before hashing. Both write
                            // paths exist on disk; verify that binding too,
                            // rather than changing either path's consensus hash.
                            let unwrapped_hash =
                                value_hash(&target.underlying().serialize(grove_version)?).unwrap();
                            combine_hash(&actual_value_hash, &unwrapped_hash).unwrap()
                        } else {
                            combined
                        }
                    }
                    // Backward-references family (two-layer scheme): a
                    // bidirectional reference's node hash is
                    // `combine(combine(inner, referrer list), end hash)` where
                    // the end hash is the terminal's LOGICAL (referrer-list
                    // stripped) hash; the item variants commit to
                    // `combine(inner, referrer list)` alone.
                    Element::BidirectionalReference(reference, _) => {
                        let hashes = element
                            .backward_references_hashes(grove_version)
                            .unwrap()?
                            .ok_or_else(|| {
                                Error::CorruptedData(
                                    "bidirectional reference without backward-references hashes"
                                        .to_string(),
                                )
                            })?;
                        let target_path = path_from_reference_path_type(
                            reference.forward_reference_path.clone(),
                            &path,
                            Some(node.key()),
                        )?;
                        let target = self.restored_reference_target(
                            target_path,
                            transaction,
                            grove_version,
                        )?;
                        let end_hash = target.logical_value_hash(grove_version).unwrap()?;
                        combine_hash(&hashes.combined, &end_hash).unwrap()
                    }
                    Element::ItemWithBackwardsReferences(..)
                    | Element::SumItemWithBackwardsReferences(..)
                    | Element::ItemWithSumItemWithBackwardsReferences(..) => {
                        element
                            .backward_references_hashes(grove_version)
                            .unwrap()?
                            .ok_or_else(|| {
                                Error::CorruptedData(
                                    "backward-references item without backward-references hashes"
                                        .to_string(),
                                )
                            })?
                            .combined
                    }
                    _ if element.element_type().has_simple_value_hash() => actual_value_hash,
                    _ if element.is_any_tree() => {
                        // Child restores (including non-Merk replay and
                        // indexed group finalization) already verified the
                        // combined binding of these bytes and child roots.
                        if !element.uses_non_merk_data_storage() {
                            let mut child_path = path.clone();
                            child_path.push(node.key().to_vec());
                            let indexed = element.is_indexed_tree().then_some(element);
                            pending.push((child_path, indexed));
                        }
                        return Ok(());
                    }
                    _ => {
                        return Err(Error::CorruptedData(
                            "unsupported restored element value hash".to_string(),
                        ));
                    }
                };
                if expected_value_hash != *node.value_hash() {
                    return Err(Error::CorruptedData(format!(
                        "restored element value hash mismatch at path {:?}, key {}",
                        super::utils::path_to_string(&path),
                        hex::encode(node.key()),
                    )));
                }
                Ok(())
            })?;

            let Some(element) = indexed_element else {
                continue;
            };
            let primary_prefix = RocksDbStorage::build_prefix(path.as_slice().into()).unwrap();
            for (axis, root_key) in indexed_element_axes(&element)? {
                let prefix =
                    RocksDbStorage::secondary_prefix_for(&primary_prefix, axis.tag()).unwrap();
                let storage = self
                    .db
                    .get_immediate_storage_context_by_subtree_prefix(prefix, transaction)
                    .unwrap();
                let secondary = Merk::open_layered_with_root_key(
                    storage,
                    root_key,
                    axis_secondary_tree_type(axis),
                    Some(&Element::value_defined_cost_for_serialized_value),
                    grove_version,
                )
                .value
                .map_err(Error::MerkError)?;
                visit_nodes(&secondary, grove_version, |node| {
                    let row = Element::deserialize(node.value_as_slice(), grove_version)?;
                    let (target_key, _) =
                        decode_axis_row_reference(&row, "state sync value hash verification")?;
                    // Index rows bind the immediate primary node, unlike
                    // ordinary references, which bind the terminal item.
                    let target_hash = merk
                        .get_value_hash(
                            target_key,
                            false,
                            Some(&Element::value_defined_cost_for_serialized_value),
                            grove_version,
                        )
                        .value
                        .map_err(Error::MerkError)?
                        .ok_or_else(|| {
                            Error::CorruptedData(
                                "restored index reference has no primary target".to_string(),
                            )
                        })?;
                    let actual =
                        combine_hash(&value_hash(node.value_as_slice()).unwrap(), &target_hash)
                            .unwrap();
                    if actual != *node.value_hash() {
                        return Err(Error::CorruptedData(
                            "restored index reference value hash mismatch".to_string(),
                        ));
                    }
                    Ok(())
                })?;
            }
        }
        Ok(())
    }
}
