//! Module for subtrees handling.
use ed::{Decode, Encode};
use merk::Op;

use crate::{Error, Merk};

/// Variants of an insertable entity
#[derive(Debug, Decode, Encode, PartialEq)]
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

    pub fn insert(&self, merk: &mut Merk, path: &[&[u8]], key: &[u8]) -> Result<(), Error> {
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

        let batch = [(merk_key, Op::Put(Element::encode(self)?))];
        merk.apply(&batch, &[]).map_err(|e| e.into())
    }
}

#[cfg(test)]
mod tests {
    use tempdir::TempDir;

    use super::*;

    #[test]
    fn test_success_insert() {
        let tmp_dir = TempDir::new("db").unwrap();
        let mut merk = Merk::open(tmp_dir.path()).unwrap();
        Element::Tree
            .insert(&mut merk, &[], b"mykey")
            .expect("expected successful insertion");
        Element::Item(b"value".to_vec())
            .insert(&mut merk, &[b"mykey"], b"another-key")
            .expect("expected successful insertion 2");

        assert_eq!(
            Element::get(&merk, &[b"mykey"], b"another-key").expect("expected successful get"),
            Element::Item(b"value".to_vec()),
        );
    }
}
