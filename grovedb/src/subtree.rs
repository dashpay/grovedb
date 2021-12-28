//! Module for subtrees handling.
//! Subtrees handling is isolated so basically this module is about adapting
//! Merk API to GroveDB needs.
use merk::Op;
use serde::{Deserialize, Serialize};
use storage::rocksdb_storage::PrefixedRocksDbStorage;
use storage::Storage;

use crate::{Error, Merk};

/// Variants of GroveDB stored entities
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum Element {
    /// An ordinary value
    Item(Vec<u8>),
    /// A reference to an object by its path
    Reference(Vec<Vec<u8>>),
    /// A subtree, contains a root hash of the underlying Merk.
    /// Hash is stored to make Merk become different when its subtrees have
    /// changed, otherwise changes won't be reflected in parent trees.
    Tree([u8; 32]),
}

impl Element {
    // TODO: improve API to avoid creation of Tree elements with uncertain state
    pub fn empty_tree() -> Element {
        Element::Tree(Default::default())
    }

    /// Get an element from Merk under a key; path should be resolved and proper
    /// Merk should be loaded by this moment
    pub fn get(merk: &Merk<PrefixedRocksDbStorage>, key: &[u8]) -> Result<Element, Error> {
        let element = bincode::deserialize(
            merk.get(&key)
                .map_err(|e| Error::CorruptedData(e.to_string()))?
                .ok_or(Error::InvalidPath("key not found in Merk"))?
                .as_slice(),
        )
        .map_err(|_| Error::CorruptedData(String::from("unable to deserialize element")))?;
        Ok(element)
    }

    /// Insert an element in Merk under a key; path should be resolved and
    /// proper Merk should be loaded by this moment
    /// If transaction is not passed, the batch will be written immediately.
    /// If transaction is passed, the operation will be committed on the transaction
    /// commit.
    pub fn insert<'a: 'b, 'b>(
        &'a self,
        merk: &mut Merk<PrefixedRocksDbStorage>,
        key: Vec<u8>,
        transaction: Option<&'b <PrefixedRocksDbStorage as Storage>::DBTransaction<'b>>
    ) -> Result<(), Error> {
        let batch_operations =
            [(
                key,
                Op::Put(bincode::serialize(self).map_err(|_| {
                    Error::CorruptedData(String::from("unable to serialize element"))
                })?),
            )];
        merk.apply(&batch_operations, &[], transaction)
            .map_err(|e| Error::CorruptedData(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use merk::test_utils::TempMerk;

    use super::*;

    #[test]
    fn test_success_insert() {
        let mut merk = TempMerk::new();
        Element::empty_tree()
            .insert(&mut merk, b"mykey".to_vec())
            .expect("expected successful insertion");
        Element::Item(b"value".to_vec())
            .insert(&mut merk, b"another-key".to_vec())
            .expect("expected successful insertion 2");

        assert_eq!(
            Element::get(&merk, b"another-key").expect("expected successful get"),
            Element::Item(b"value".to_vec()),
        );
    }
}
