//! Bind restored element bytes to the value hashes authenticated by Merk chunks.
//!
//! References can point into subtrees that arrive later, so their bindings
//! must be checked after discovery completes and before the final commit.

use grovedb_merk::{
    element::costs::ElementCostExtensions,
    tree::{combine_hash, kv::ValueDefinedCostType, value_hash, TreeNode},
    Merk,
};
use grovedb_storage::{rocksdb_storage::RocksDbStorage, RawIterator, Storage, StorageContext};
use grovedb_version::version::GroveVersion;

use crate::{
    operations::indexed_tree::{
        axis_secondary_tree_type, decode_axis_row_reference, indexed_element_axes,
    },
    reference_path::path_from_reference_path_type,
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
                        let target = self
                            .follow_reference(
                                target_path.as_slice().into(),
                                false,
                                Some(transaction),
                                grove_version,
                            )
                            .value?;
                        let target_hash = value_hash(&target.serialize(grove_version)?).unwrap();
                        combine_hash(&actual_value_hash, &target_hash).unwrap()
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
