use rs_merkle::{algorithms::Sha256, MerkleTree};
use merk::{self, Merk};



#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("unable to open Merk db")]
    MerkError(merk::Error),
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
            Merk::open("./test1.db")?,
            Merk::open("./test2.db")?,
            Merk::open("./test3.db")?,
            Merk::open("./test4.db")?,
            Merk::open("./test5.db")?,
            Merk::open("./test6.db")?,
        ];
        let leaves: Vec<[u8; 32]> = subtrees
            .iter()
            .map(|x| x.root_hash())
            .collect();
        Ok(GroveDb {
            root_tree: MerkleTree::<Sha256>::from_leaves(&leaves),
            subtrees,
        })
    }

    pub fn insert(&mut self) -> ! {
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
