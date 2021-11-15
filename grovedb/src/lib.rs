#![feature(trivial_bounds)]
use ed::Encode;
use merk::{self, Merk};
use rs_merkle::{algorithms::Sha256, Hasher, MerkleTree};
use subtree::Element;
mod subtree;

const MERK_DIR: &str = "./grove.db";

// Root tree has hardcoded leafs; each of them is `pub` to be easily used in
// `path` arg
pub const COMMON_TREE_KEY: &[u8] = b"common";
pub const IDENTITIES_TREE_KEY: &[u8] = b"identities";
pub const PUBLIC_KEYS_TO_IDENTITY_IDS_TREE_KEY: &[u8] = b"publicKeysToIdentityIDs";
pub const DATA_CONTRACTS_TREE_KEY: &[u8] = b"dataContracts";
pub const SPENT_ASSET_LOCK_TRANSACTIONS_TREE_KEY: &[u8] = b"spentAssetLockTransactions";
const SUBTREES: [&[u8]; 5] = [
    COMMON_TREE_KEY,
    IDENTITIES_TREE_KEY,
    PUBLIC_KEYS_TO_IDENTITY_IDS_TREE_KEY,
    DATA_CONTRACTS_TREE_KEY,
    SPENT_ASSET_LOCK_TRANSACTIONS_TREE_KEY,
];

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("unable to open Merk db")]
    MerkError(merk::Error),
    #[error("invalid path")]
    InvalidPath(&'static str),
    #[error("unable to decode")]
    EdError(#[from] ed::Error),
    #[error("cyclic reference path")]
    CyclicReferencePath,
}

impl From<merk::Error> for Error {
    fn from(e: merk::Error) -> Self {
        Error::MerkError(e)
    }
}

pub struct GroveDb {
    root_tree: MerkleTree<Sha256>,
    subtrees_merk: Merk,
}

impl GroveDb {
    pub fn new() -> Result<Self, Error> {
        let mut subtrees_merk = Merk::open(MERK_DIR)?;
        let mut leaves = Vec::with_capacity(SUBTREES.len());
        // Populate Merk with root tree's leafs if no previous Merk data found
        for subtree_key in SUBTREES {
            let element = match Element::get(&subtrees_merk, &[], subtree_key) {
                Err(Error::InvalidPath(_)) => {
                    // no leaf for a subtree in a root tree, will create a record
                    let element = Element::Tree;
                    element.insert(&mut subtrees_merk, &[], subtree_key)?;
                    element
                }
                Ok(element) => element,
                e => e?,
            };
            // TODO
            leaves.push(todo!("need to insert node hash"));
        }
        Ok(GroveDb {
            root_tree: MerkleTree::<Sha256>::from_leaves(&leaves),
            subtrees_merk,
        })
    }

    pub fn insert(
        &mut self,
        path: &[&[u8]],
        key: &[u8],
        element: subtree::Element,
    ) -> Result<(), Error> {
        todo!()
    }

    pub fn get(&self, path: &[&[u8]], key: &[u8]) -> Result<subtree::Element, Error> {
        todo!()
    }

    pub fn proof(&self) -> ! {
        todo!()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_init() {
        GroveDb::new().expect("empty tree is ok");
    }
}
