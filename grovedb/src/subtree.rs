//! Module for subtrees handling.
use ed::{Decode, Encode};

use super::{Error, Merk};

/// Variants of an insertable entity
#[derive(Debug, Decode, Encode)]
pub enum Element {
    /// An ordinary value
    Item(Vec<u8>),
    /// A reference to an object
    Reference(Vec<u8>),
    /// A subtree
    Tree,
}

// TODO: resolve references

impl Element {
    /// Helper method to short-circuit out in case a tree is expected
    fn is_tree(&self) -> Result<(), Error> {
        match self {
            Element::Tree => Ok(()),
            _ => Err(Error::InvalidPath),
        }
    }

    pub fn get(merk: &Merk, path: &[&[u8]], key: &[u8]) -> Result<Element, Error> {
        // We'll iterate over path accumulating RocksDB key to retrieve the data,
        // validating the path while doing so
        let mut merk_key = Vec::new();
        for p in path {
            merk_key.extend(p.into_iter());
            let element =
                Element::decode(merk.get(&merk_key)?.ok_or(Error::InvalidPath)?.as_slice())?;
            element.is_tree()?;
        }
        merk_key.extend(key);
        Ok(Element::decode(
            merk.get(&merk_key)?.ok_or(Error::InvalidPath)?.as_slice(),
        )?)
    }

    pub fn insert(&self, merk: &Merk, path: &[&[u8]], key: &[u8]) -> Result<(), Error> {
        // check if a tree was inserted by the path
        if let Some((tree_key, tree_path)) = path.split_last() {
            Element::get(merk, tree_path, tree_key)?.is_tree()?;
        }
        if path.len() == 1 {
            Element::get(
                merk,
                &[],
                path.first().expect("expected the path of length of 1"),
            )?
            .is_tree()?;
        }

        let mut merk_key = path.iter().fold(Vec::<u8>::new(), |mut acc, p| {
            acc.extend(p.into_iter());
            acc
        });
        merk_key.extend(key);

        // TODO: insert into Merk
        todo!()
    }
}
