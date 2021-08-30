pub trait GroveInnerTree {
  fn get_id(&self) -> &str;

  fn insert(&self, key: [u8; 32], value: Option<[u8; 32]>) -> Result<Option<[u8; 32]>, String>;

  fn get(&self, key: [u8; 32]) -> Option<[u8; 32]>;

  fn get_root_hash(&self) -> RootHash;
}

pub enum RootHash {
  ByteHash32([u8; 32]),
  ByteHash64([u8; 64]),
}

impl From<[u8; 32]> for RootHash {
  fn from(value: [u8; 32]) -> RootHash {
    RootHash::ByteHash32(value)
  }
}

impl From<[u8; 64]> for RootHash {
  fn from(value: [u8; 64]) -> RootHash {
    RootHash::ByteHash64(value)
  }
}

impl From<RootHash> for [u8; 32] {
  fn from(value: RootHash) -> [u8; 32] {
    return match value {
      RootHash::ByteHash32(array) => array,
      RootHash::ByteHash64(_) => panic!("can not convert [u8; 64] to [u8; 32]")
    }
  }
}

impl From<RootHash> for [u8; 64] {
  fn from(value: RootHash) -> [u8; 64] {
    return match value {
      RootHash::ByteHash32(_) => panic!("can not convert [u8; 32] to [u8; 64]"),
      RootHash::ByteHash64(array) => array
    }
  }
}