#![feature(trivial_bounds)]
use merk::{self, Merk};
use rs_merkle::{algorithms::Sha256, MerkleTree};
mod subtree;
use std::path::Path;

const SUBTREES_DIR: &str = "./subtrees";

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("unable to open Merk db")]
    MerkError(merk::Error),
    #[error("invalid path")]
    InvalidPath,
    #[error("unable to decode")]
    EdError(#[from] ed::Error),
}

impl From<merk::Error> for Error {
    fn from(e: merk::Error) -> Self {
        Error::MerkError(e)
    }
}

pub struct GroveDb {
    root_tree: MerkleTree<Sha256>,
    subtrees: Vec<Merk>,
}

impl GroveDb {
    pub fn new() -> Result<Self, Error> {
        let subtrees = vec![
            Merk::open(Path::new(SUBTREES_DIR).join("test1.db"))?,
            Merk::open(Path::new(SUBTREES_DIR).join("test2.db"))?,
            Merk::open(Path::new(SUBTREES_DIR).join("test3.db"))?,
            Merk::open(Path::new(SUBTREES_DIR).join("test4.db"))?,
            Merk::open(Path::new(SUBTREES_DIR).join("test5.db"))?,
            Merk::open(Path::new(SUBTREES_DIR).join("test6.db"))?,
        ];
        let leaves: Vec<[u8; 32]> = subtrees.iter().map(|x| x.root_hash()).collect();
        Ok(GroveDb {
            root_tree: MerkleTree::<Sha256>::from_leaves(&leaves),
            subtrees,
        })
    }

    // TODO: as root tree structure is known in advance it may be reasonable to have
    // separate methods for each root tree leaf or other way to dispatch (like enum
    // arg); Let's have only one of these methods for now
    // TODO: autocreate option
    pub fn insert_test1(&mut self, path: &[&[u8]], key: &[u8], element: subtree::Element) -> ! {
        todo!()
    }

    pub fn get_test1(&self, path: &[&[u8]], key: &[u8]) -> Option<subtree::Element> {
        // A merk tree is a leaf of root tree and
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
