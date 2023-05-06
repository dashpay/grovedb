//! Utilities module for path library.

use std::hash::{Hash, Hasher};
use std::ops::Deref;

/// A smart pointer that follows the semantics of [Cow](std::borrow::Cow) except
/// provides no means for mutability and thus doesn't require [Clone].
#[derive(Debug)]
pub enum CowLike<'b> {
    Owned(Vec<u8>),
    Borrowed(&'b [u8]),
}

impl Deref for CowLike<'_> {
    type Target = [u8];

    fn deref(&self) -> &Self::Target {
        match self {
            Self::Owned(v) => v.as_slice(),
            Self::Borrowed(s) => s,
        }
    }
}

impl Hash for CowLike<'_> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.deref().hash(state);
    }
}

impl<'b> From<Vec<u8>> for CowLike<'static> {
    fn from(value: Vec<u8>) -> Self {
        Self::Owned(value)
    }
}

impl<'b> From<&'b [u8]> for CowLike<'b> {
    fn from(value: &'b [u8]) -> Self {
        Self::Borrowed(value)
    }
}

#[cfg(test)]
pub(crate) fn calculate_hash<T: Hash>(t: &T) -> u64 {
    use std::collections::hash_map::DefaultHasher;

    let mut s = DefaultHasher::new();
    t.hash(&mut s);
    s.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cowlike_hashes() {
        let owned = CowLike::Owned(vec![1u8, 3, 3, 7]);
        let borrowed = CowLike::Borrowed(&[1u8, 3, 3, 7]);

        assert_eq!(calculate_hash(&owned), calculate_hash(&borrowed));
    }
}
