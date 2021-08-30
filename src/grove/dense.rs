use super::inner::{ GroveInnerTree, RootHash };

use std::hash::Hasher;

use blake2::{ Blake2b, Digest };

use merkletree::hash::{ Algorithm };
use merkletree::merkle::{ MerkleTree, Element };
use merkletree::store::VecStore;

pub struct DenseTree {
  id: String,
  tree: Option<MerkleTree<HashArray64, Blake2Algorithm, VecStore<HashArray64>>>,
}

impl DenseTree {
  pub fn new(id: String, hashes: Vec<[u8; 64]>) -> DenseTree {
    println!("length of hashes: {:?}", hashes.len());
    return match MerkleTree::try_from_iter(hashes.iter().map(|v| HashArray64(*v)).into_iter().map(Ok)) {
      Ok(tree) => DenseTree {
        id,
        tree: Some(tree),
      },
      Err(e) => {
        println!("error: {:?}", e);
        DenseTree {
          id,
          tree: None,
        }
      }
    }
  }
}

impl GroveInnerTree for DenseTree {
  fn get_id(&self) -> &str {
    &self.id
  }

  fn insert(&self, key: [u8; 32], value: Option<[u8; 32]>) -> Result<Option<[u8; 32]>, String> {
    Ok(None)
  }

  fn get(&self, key: [u8; 32]) -> Option<[u8; 32]> {
    None
  }

  fn get_root_hash(&self) -> RootHash {
    return match &self.tree {
      Some(tree) => tree.root().0.into(),
      None => {
        println!("well, no tree found");
        [0; 64].into()
      },
    }
  }
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct HashArray64([u8; 64]);

impl Default for HashArray64 {
  fn default() -> HashArray64 {
    HashArray64([0; 64])
  }
}

impl AsRef<[u8]> for HashArray64 {
  fn as_ref(&self) -> &[u8] {
    &self.0
  }
}

impl Clone for HashArray64 {
  fn clone(&self) -> HashArray64 {
    let mut clone: [u8; 64] = [0; 64];
    clone.clone_from_slice(&self.0);
    HashArray64(clone)
  }
}

impl Element for HashArray64 {
  fn byte_len() -> usize {
      64
  }

  fn from_slice(bytes: &[u8]) -> Self {
      if bytes.len() != 64 {
          panic!("invalid length {}, expected 64", bytes.len());
      }
      let mut clone: [u8; 64] = [0; 64];
      clone.clone_from_slice(bytes);
      HashArray64(clone)
  }

  fn copy_to_slice(&self, bytes: &mut [u8]) {
      bytes.copy_from_slice(&self.0);
  }
}

pub struct Blake2Algorithm {
  inner: Box<Blake2b>,
}

impl Blake2Algorithm {
  pub fn new() -> Blake2Algorithm {
    Blake2Algorithm {
      inner: Box::new(Blake2b::new())
    }
  }
}

impl Default for Blake2Algorithm {
  fn default() -> Blake2Algorithm {
    Blake2Algorithm::new()
  }
}

impl Hasher for Blake2Algorithm {
  #[inline]
  fn write(&mut self, msg: &[u8]) {
      self.inner.update(msg)
  }

  #[inline]
  fn finish(&self) -> u64 {
      unimplemented!()
  }
}

impl Algorithm<HashArray64> for Blake2Algorithm {
  #[inline]
  fn hash(&mut self) -> HashArray64 {
    let output = self.inner.clone().finalize();
    let mut copy: [u8; 64] = [0; 64];
    copy.clone_from_slice(output.as_slice());
    HashArray64(copy)
  }

  #[inline]
  fn reset(&mut self) {
      self.inner.reset();
  }
}
