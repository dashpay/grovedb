// MIT LICENSE
//
// Copyright (c) 2021 Dash Core Group
//
// Permission is hereby granted, free of charge, to any
// person obtaining a copy of this software and associated
// documentation files (the "Software"), to deal in the
// Software without restriction, including without
// limitation the rights to use, copy, modify, merge,
// publish, distribute, sublicense, and/or sell copies of
// the Software, and to permit persons to whom the Software
// is furnished to do so, subject to the following
// conditions:
//
// The above copyright notice and this permission notice
// shall be included in all copies or substantial portions
// of the Software.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF
// ANY KIND, EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED
// TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A
// PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT
// SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY
// CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION
// OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR
// IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER
// DEALINGS IN THE SOFTWARE.

//! Merk tree link

#[cfg(feature = "full")]
use std::io::{Read, Write};

#[cfg(feature = "full")]
use ed::{Decode, Encode, Result, Terminated};
#[cfg(feature = "full")]
use integer_encoding::{VarInt, VarIntReader, VarIntWriter};

#[cfg(feature = "full")]
use super::{hash::CryptoHash, Tree};
#[cfg(feature = "full")]
use crate::HASH_LENGTH_U32;

// TODO: optimize memory footprint

#[cfg(feature = "full")]
/// Represents a reference to a child tree node. Links may or may not contain
/// the child's `Tree` instance (storing its key if not).
#[derive(Clone, Debug, PartialEq)]
pub enum Link {
    /// Represents a child tree node which has been pruned from memory, only
    /// retaining a reference to it (its key). The child node can always be
    /// fetched from the backing store by this key when necessary.
    Reference {
        /// Hash
        hash: CryptoHash,
        /// Child heights
        child_heights: (u8, u8),
        /// Key
        key: Vec<u8>,
        /// Sum
        sum: Option<i64>,
    },

    /// Represents a tree node which has been modified since the `Tree`'s last
    /// hash computation. The child's hash is not stored since it has not yet
    /// been recomputed. The child's `Tree` instance is stored in the link.
    #[rustfmt::skip]
    Modified {
        /// Pending writes
        pending_writes: usize, // TODO: rename to `pending_hashes`
        /// Child heights
        child_heights: (u8, u8),
        /// Tree
        tree: Tree
    },

    /// Represents a tree node which has been modified since the `Tree`'s last
    /// commit, but which has an up-to-date hash. The child's `Tree` instance is
    /// stored in the link.
    Uncommitted {
        /// Hash
        hash: CryptoHash,
        /// Child heights
        child_heights: (u8, u8),
        /// Tree
        tree: Tree,
        /// Sum
        sum: Option<i64>,
    },

    /// Represents a tree node which has not been modified, has an up-to-date
    /// hash, and which is being retained in memory.
    Loaded {
        /// Hash
        hash: CryptoHash,
        /// Child heights
        child_heights: (u8, u8),
        /// Tree
        tree: Tree,
        /// Sum
        sum: Option<i64>,
    },
}

#[cfg(feature = "full")]
impl Link {
    /// Creates a `Link::Modified` from the given `Tree`.
    #[inline]
    pub const fn from_modified_tree(tree: Tree) -> Self {
        let pending_writes = 1 + tree.child_pending_writes(true) + tree.child_pending_writes(false);

        Self::Modified {
            pending_writes,
            child_heights: tree.child_heights(),
            tree,
        }
    }

    /// Creates a `Link::Modified` from the given tree, if any. If `None`,
    /// returns `None`.
    pub fn maybe_from_modified_tree(maybe_tree: Option<Tree>) -> Option<Self> {
        maybe_tree.map(Self::from_modified_tree)
    }

    /// Returns `true` if the link is of the `Link::Reference` variant.
    #[inline]
    pub const fn is_reference(&self) -> bool {
        matches!(self, Link::Reference { .. })
    }

    /// Returns `true` if the link is of the `Link::Modified` variant.
    #[inline]
    pub const fn is_modified(&self) -> bool {
        matches!(self, Link::Modified { .. })
    }

    /// Returns `true` if the link is of the `Link::Uncommitted` variant.
    #[inline]
    pub const fn is_uncommitted(&self) -> bool {
        matches!(self, Link::Uncommitted { .. })
    }

    /// Returns `true` if the link is of the `Link::Loaded` variant.
    #[inline]
    pub const fn is_stored(&self) -> bool {
        matches!(self, Link::Loaded { .. })
    }

    /// Returns the key of the tree referenced by this link, as a slice.
    #[inline]
    pub fn key(&self) -> &[u8] {
        match self {
            Link::Reference { key, .. } => key.as_slice(),
            Link::Modified { tree, .. } => tree.key(),
            Link::Uncommitted { tree, .. } => tree.key(),
            Link::Loaded { tree, .. } => tree.key(),
        }
    }

    /// Returns the `Tree` instance of the tree referenced by the link. If the
    /// link is of variant `Link::Reference`, the returned value will be `None`.
    #[inline]
    pub const fn tree(&self) -> Option<&Tree> {
        match self {
            // TODO: panic for Reference, don't return Option?
            Link::Reference { .. } => None,
            Link::Modified { tree, .. } => Some(tree),
            Link::Uncommitted { tree, .. } => Some(tree),
            Link::Loaded { tree, .. } => Some(tree),
        }
    }

    /// Returns the hash of the tree referenced by the link. Panics if link is
    /// of variant `Link::Modified` since we have not yet recomputed the tree's
    /// hash.
    #[inline]
    pub const fn hash(&self) -> &CryptoHash {
        match self {
            Link::Modified { .. } => panic!("Cannot get hash from modified link"),
            Link::Reference { hash, .. } => hash,
            Link::Uncommitted { hash, .. } => hash,
            Link::Loaded { hash, .. } => hash,
        }
    }

    /// Returns the sum of the tree referenced by the link. Panics if link is
    /// of variant `Link::Modified` since we have not yet recomputed the tree's
    /// hash.
    #[inline]
    pub const fn sum(&self) -> Option<i64> {
        match self {
            Link::Modified { .. } => panic!("Cannot get hash from modified link"),
            Link::Reference { sum, .. } => *sum,
            Link::Uncommitted { sum, .. } => *sum,
            Link::Loaded { sum, .. } => *sum,
        }
    }

    /// Returns the height of the children of the tree referenced by the link,
    /// if any (note: not the height of the referenced tree itself). Return
    /// value is `(left_child_height, right_child_height)`.
    #[inline]
    pub const fn height(&self) -> u8 {
        const fn max(a: u8, b: u8) -> u8 {
            if a >= b {
                a
            } else {
                b
            }
        }

        let (left_height, right_height) = match self {
            Link::Reference { child_heights, .. } => *child_heights,
            Link::Modified { child_heights, .. } => *child_heights,
            Link::Uncommitted { child_heights, .. } => *child_heights,
            Link::Loaded { child_heights, .. } => *child_heights,
        };
        1 + max(left_height, right_height)
    }

    /// Returns the balance factor of the tree referenced by the link.
    #[inline]
    pub const fn balance_factor(&self) -> i8 {
        let (left_height, right_height) = match self {
            Link::Reference { child_heights, .. } => *child_heights,
            Link::Modified { child_heights, .. } => *child_heights,
            Link::Uncommitted { child_heights, .. } => *child_heights,
            Link::Loaded { child_heights, .. } => *child_heights,
        };
        right_height as i8 - left_height as i8
    }

    /// Consumes the link and converts to variant `Link::Reference`. Panics if
    /// the link is of variant `Link::Modified` or `Link::Uncommitted`.
    #[inline]
    pub fn into_reference(self) -> Self {
        match self {
            Link::Reference { .. } => self,
            Link::Modified { .. } => panic!("Cannot prune Modified tree"),
            Link::Uncommitted { .. } => panic!("Cannot prune Uncommitted tree"),
            Link::Loaded {
                hash,
                sum,
                child_heights,
                tree,
            } => Self::Reference {
                hash,
                sum,
                child_heights,
                key: tree.take_key(),
            },
        }
    }

    #[inline]
    /// Return heights of children of the Link as mutable tuple
    pub(crate) fn child_heights_mut(&mut self) -> &mut (u8, u8) {
        match self {
            Link::Reference {
                ref mut child_heights,
                ..
            } => child_heights,
            Link::Modified {
                ref mut child_heights,
                ..
            } => child_heights,
            Link::Uncommitted {
                ref mut child_heights,
                ..
            } => child_heights,
            Link::Loaded {
                ref mut child_heights,
                ..
            } => child_heights,
        }
    }

    // Costs for operations within a single merk
    #[inline]
    /// Encoded link size
    pub const fn encoded_link_size(not_prefixed_key_len: u32, is_sum_tree: bool) -> u32 {
        let sum_tree_cost = if is_sum_tree { 8 } else { 0 };
        // Links are optional values that represent the right or left node for a given
        // 1 byte to represent key_length (this is a u8)
        // key_length to represent the actual key
        // 32 bytes for the hash of the node
        // 1 byte for the left child height
        // 1 byte for the right child height
        // 1 byte for the sum tree option
        not_prefixed_key_len + HASH_LENGTH_U32 + 4 + sum_tree_cost
    }

    /// The encoding cost is always 8 bytes for the sum instead of a varint
    #[inline]
    pub fn encoding_cost(&self) -> Result<usize> {
        debug_assert!(self.key().len() < 256, "Key length must be less than 256");

        Ok(match self {
            Link::Reference { key, sum, .. } => match sum {
                None => key.len() + 36, // 1 + HASH_LENGTH + 2 + 1,
                Some(_sum_value) => {
                    // 1 for key len
                    // key_len for keys
                    // 32 for hash
                    // 2 for child heights
                    // 1 to represent presence of sum value
                    //    if above is 1, then
                    //    1 for sum len
                    //    sum_len for sum vale
                    key.len() + 44 // 1 + 32 + 2 + 1 + 8
                }
            },
            Link::Modified { .. } => panic!("No encoding for Link::Modified"),
            Link::Uncommitted { tree, sum, .. } | Link::Loaded { tree, sum, .. } => match sum {
                None => tree.key().len() + 36, // 1 + 32 + 2 + 1,
                Some(sum_value) => {
                    let _encoded_sum_value = sum_value.encode_var_vec();
                    tree.key().len() + 44 // 1 + 32 + 2 + 1 + 8
                }
            },
        })
    }
}

#[cfg(feature = "full")]
impl Encode for Link {
    #[inline]
    fn encode_into<W: Write>(&self, out: &mut W) -> Result<()> {
        let (hash, sum, key, (left_height, right_height)) = match self {
            Link::Reference {
                hash,
                sum,
                key,
                child_heights,
            } => (hash, sum, key.as_slice(), child_heights),
            Link::Loaded {
                hash,
                sum,
                tree,
                child_heights,
            } => (hash, sum, tree.key(), child_heights),
            Link::Uncommitted {
                hash,
                sum,
                tree,
                child_heights,
            } => (hash, sum, tree.key(), child_heights),

            Link::Modified { .. } => panic!("No encoding for Link::Modified"),
        };

        debug_assert!(key.len() < 256, "Key length must be less than 256");

        out.write_all(&[key.len() as u8])?;
        out.write_all(key)?;

        out.write_all(hash)?;

        out.write_all(&[*left_height, *right_height])?;

        match sum {
            None => {
                out.write_all(&[0])?;
            }
            Some(sum_value) => {
                out.write_all(&[1])?;
                out.write_varint(sum_value.to_owned())?;
            }
        }

        Ok(())
    }

    #[inline]
    fn encoding_length(&self) -> Result<usize> {
        debug_assert!(self.key().len() < 256, "Key length must be less than 256");

        Ok(match self {
            Link::Reference { key, sum, .. } => match sum {
                None => key.len() + 36, // 1 + 32 + 2 + 1
                Some(sum_value) => {
                    let encoded_sum_value = sum_value.encode_var_vec();
                    // 1 for key len
                    // key_len for keys
                    // 32 for hash
                    // 2 for child heights
                    // 1 to represent presence of sum value
                    //    if above is 1, then
                    //    1 for sum len
                    //    sum_len for sum vale
                    key.len() + encoded_sum_value.len() + 36 // 1 + 32 + 2 + 1
                }
            },
            Link::Modified { .. } => panic!("No encoding for Link::Modified"),
            Link::Uncommitted { tree, sum, .. } | Link::Loaded { tree, sum, .. } => match sum {
                None => tree.key().len() + 36, // 1 + 32 + 2 + 1
                Some(sum_value) => {
                    let encoded_sum_value = sum_value.encode_var_vec();
                    tree.key().len() + encoded_sum_value.len() + 36 // 1 + 32 + 2 + 1
                }
            },
        })
    }
}

#[cfg(feature = "full")]
impl Link {
    #[inline]
    fn default_reference() -> Self {
        Self::Reference {
            key: Vec::with_capacity(64),
            hash: Default::default(),
            sum: None,
            child_heights: (0, 0),
        }
    }
}

#[cfg(feature = "full")]
impl Decode for Link {
    #[inline]
    fn decode<R: Read>(input: R) -> Result<Self> {
        let mut link = Self::default_reference();
        Self::decode_into(&mut link, input)?;
        Ok(link)
    }

    #[inline]
    fn decode_into<R: Read>(&mut self, mut input: R) -> Result<()> {
        if !self.is_reference() {
            // don't create new struct if self is already Link::Reference,
            // so we can re-use the key vec
            *self = Self::default_reference();
        }

        if let Link::Reference {
            ref mut sum,
            ref mut key,
            ref mut hash,
            ref mut child_heights,
        } = self
        {
            let length = read_u8(&mut input)? as usize;

            key.resize(length, 0);
            input.read_exact(key.as_mut())?;

            input.read_exact(&mut hash[..])?;

            child_heights.0 = read_u8(&mut input)?;
            child_heights.1 = read_u8(&mut input)?;

            let has_sum = read_u8(&mut input)?;
            *sum = match has_sum {
                0 => None,
                1 => {
                    let encoded_sum: i64 = input.read_varint()?;
                    Some(encoded_sum)
                }
                _ => return Err(ed::Error::UnexpectedByte(55)),
            };
        } else {
            unreachable!()
        }

        Ok(())
    }
}

#[cfg(feature = "full")]
impl Terminated for Link {}

#[cfg(feature = "full")]
#[inline]
fn read_u8<R: Read>(mut input: R) -> Result<u8> {
    let mut length = [0];
    input.read_exact(length.as_mut())?;
    Ok(length[0])
}

#[cfg(feature = "full")]
#[cfg(test)]
mod test {
    use super::{
        super::{hash::NULL_HASH, Tree},
        *,
    };
    use crate::TreeFeatureType::BasicMerk;

    #[test]
    fn from_modified_tree() {
        let tree = Tree::new(vec![0], vec![1], None, BasicMerk).unwrap();
        let link = Link::from_modified_tree(tree);
        assert!(link.is_modified());
        assert_eq!(link.height(), 1);
        assert_eq!(link.tree().expect("expected tree").key(), &[0]);
        if let Link::Modified { pending_writes, .. } = link {
            assert_eq!(pending_writes, 1);
        } else {
            panic!("Expected Link::Modified");
        }
    }

    #[test]
    fn maybe_from_modified_tree() {
        let link = Link::maybe_from_modified_tree(None);
        assert!(link.is_none());

        let tree = Tree::new(vec![0], vec![1], None, BasicMerk).unwrap();
        let link = Link::maybe_from_modified_tree(Some(tree));
        assert!(link.expect("expected link").is_modified());
    }

    #[test]
    fn types() {
        let hash = NULL_HASH;
        let sum = None;
        let child_heights = (0, 0);
        let pending_writes = 1;
        let key = vec![0];
        let tree = || Tree::new(vec![0], vec![1], None, BasicMerk).unwrap();

        let reference = Link::Reference {
            hash,
            sum,
            child_heights,
            key,
        };
        let modified = Link::Modified {
            pending_writes,
            child_heights,
            tree: tree(),
        };
        let uncommitted = Link::Uncommitted {
            hash,
            sum,
            child_heights,
            tree: tree(),
        };
        let loaded = Link::Loaded {
            hash,
            sum,
            child_heights,
            tree: tree(),
        };

        assert!(reference.is_reference());
        assert!(!reference.is_modified());
        assert!(!reference.is_uncommitted());
        assert!(!reference.is_stored());
        assert!(reference.tree().is_none());
        assert_eq!(reference.hash(), &[0; 32]);
        assert_eq!(reference.height(), 1);
        assert!(reference.into_reference().is_reference());

        assert!(!modified.is_reference());
        assert!(modified.is_modified());
        assert!(!modified.is_uncommitted());
        assert!(!modified.is_stored());
        assert!(modified.tree().is_some());
        assert_eq!(modified.height(), 1);

        assert!(!uncommitted.is_reference());
        assert!(!uncommitted.is_modified());
        assert!(uncommitted.is_uncommitted());
        assert!(!uncommitted.is_stored());
        assert!(uncommitted.tree().is_some());
        assert_eq!(uncommitted.hash(), &[0; 32]);
        assert_eq!(uncommitted.height(), 1);

        assert!(!loaded.is_reference());
        assert!(!loaded.is_modified());
        assert!(!loaded.is_uncommitted());
        assert!(loaded.is_stored());
        assert!(loaded.tree().is_some());
        assert_eq!(loaded.hash(), &[0; 32]);
        assert_eq!(loaded.height(), 1);
        assert!(loaded.into_reference().is_reference());
    }

    #[test]
    #[should_panic]
    fn modified_hash() {
        Link::Modified {
            pending_writes: 1,
            child_heights: (1, 1),
            tree: Tree::new(vec![0], vec![1], None, BasicMerk).unwrap(),
        }
        .hash();
    }

    #[test]
    #[should_panic]
    fn modified_into_reference() {
        Link::Modified {
            pending_writes: 1,
            child_heights: (1, 1),
            tree: Tree::new(vec![0], vec![1], None, BasicMerk).unwrap(),
        }
        .into_reference();
    }

    #[test]
    #[should_panic]
    fn uncommitted_into_reference() {
        Link::Uncommitted {
            hash: [1; 32],
            sum: None,
            child_heights: (1, 1),
            tree: Tree::new(vec![0], vec![1], None, BasicMerk).unwrap(),
        }
        .into_reference();
    }

    #[test]
    fn encode_link() {
        let link = Link::Reference {
            key: vec![1, 2, 3],
            sum: None,
            child_heights: (123, 124),
            hash: [55; 32],
        };
        assert_eq!(link.encoding_length().unwrap(), 39);

        let mut bytes = vec![];
        link.encode_into(&mut bytes).unwrap();
        assert_eq!(
            bytes,
            vec![
                3, 1, 2, 3, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55,
                55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 123, 124, 0
            ]
        );
    }

    #[test]
    fn encode_link_with_sum() {
        let link = Link::Reference {
            key: vec![1, 2, 3],
            sum: Some(50),
            child_heights: (123, 124),
            hash: [55; 32],
        };
        assert_eq!(link.encoding_length().unwrap(), 40);

        let mut bytes = vec![];
        link.encode_into(&mut bytes).unwrap();

        assert_eq!(link.encoding_length().unwrap(), bytes.len());
        assert_eq!(
            bytes,
            vec![
                3, 1, 2, 3, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55,
                55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 123, 124, 1, 100,
            ]
        );
    }

    #[test]
    #[should_panic]
    fn encode_link_long_key() {
        let link = Link::Reference {
            key: vec![123; 300],
            sum: None,
            child_heights: (123, 124),
            hash: [55; 32],
        };
        let mut bytes = vec![];
        link.encode_into(&mut bytes).unwrap();
    }

    #[test]
    fn decode_link() {
        let bytes = vec![
            3, 1, 2, 3, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55,
            55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 123, 124, 0,
        ];
        let link = Link::decode(bytes.as_slice()).expect("expected to decode a link");
        assert_eq!(link.sum(), None);
    }
}
