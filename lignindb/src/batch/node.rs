//! A node as a batch holds it between the call that stages it and the
//! application of the batch.

use std::{
    borrow::Borrow,
    cmp::Ordering,
    fmt,
    hash::{Hash as StdHash, Hasher},
    ops::Deref,
};

use lignin_format::tree_page::Hash;
use lignin_tree::{hash::Stale, node::NodeHandle};

use crate::txn::{Hosted, LinkPut, NodePut, ValuePut};

/// Bytes that are kept in place where there are no more than `N` of them
/// and no more than 255, and behind a box otherwise.
///
/// It reads as the slice of its bytes, and is compared, ordered and hashed
/// as that slice is, so a map that is keyed by it is asked with a slice.
/// The bytes of a staged node are kept so: a batch asks for no memory for
/// a key, a feature or an aggregate that fits.
#[derive(Clone)]
pub struct InlineBytes<const N: usize>(Held<N>);

#[derive(Clone)]
enum Held<const N: usize> {
    Here { len: u8, bytes: [u8; N] },
    Boxed(Box<[u8]>),
}

impl<const N: usize> InlineBytes<N> {
    /// A copy of `bytes`.
    pub fn new(bytes: &[u8]) -> Self {
        match u8::try_from(bytes.len()) {
            Ok(len) if bytes.len() <= N => {
                let mut here = [0; N];
                here[..bytes.len()].copy_from_slice(bytes);
                Self(Held::Here { len, bytes: here })
            }
            _ => Self(Held::Boxed(bytes.into())),
        }
    }

    /// Whether the bytes are kept in place, and not behind a box.
    pub const fn is_inline(&self) -> bool {
        matches!(self.0, Held::Here { .. })
    }

    /// The bytes.
    pub fn as_slice(&self) -> &[u8] {
        match &self.0 {
            Held::Here { len, bytes } => &bytes[..usize::from(*len)],
            Held::Boxed(bytes) => bytes,
        }
    }
}

impl<const N: usize> Deref for InlineBytes<N> {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        self.as_slice()
    }
}

impl<const N: usize> AsRef<[u8]> for InlineBytes<N> {
    fn as_ref(&self) -> &[u8] {
        self
    }
}

impl<const N: usize> Borrow<[u8]> for InlineBytes<N> {
    fn borrow(&self) -> &[u8] {
        self
    }
}

impl<const N: usize> From<&[u8]> for InlineBytes<N> {
    fn from(bytes: &[u8]) -> Self {
        Self::new(bytes)
    }
}

impl<const N: usize> fmt::Debug for InlineBytes<N> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&**self, f)
    }
}

impl<const N: usize> PartialEq for InlineBytes<N> {
    fn eq(&self, other: &Self) -> bool {
        **self == **other
    }
}

impl<const N: usize> Eq for InlineBytes<N> {}

impl<const N: usize> PartialOrd for InlineBytes<N> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl<const N: usize> Ord for InlineBytes<N> {
    fn cmp(&self, other: &Self) -> Ordering {
        (**self).cmp(&**other)
    }
}

impl<const N: usize> StdHash for InlineBytes<N> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        (**self).hash(state);
    }
}

/// A key as a batch holds it: in place up to 46 bytes, which makes it 48
/// bytes wide.
pub type StagedKey = InlineBytes<46>;

/// The feature of a node or the aggregate of a link as a batch holds it:
/// in place up to 30 bytes, which makes it 32 bytes wide. The longest that
/// a tree page takes is 25 bytes (`tree-page.md` TP12).
pub type StagedFeature = InlineBytes<30>;

/// A link of a staged node: the child by its key, as Merk holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagedLink {
    /// The child's key.
    pub key: StagedKey,
    /// The child's hash.
    pub hash: Hash,
    /// The heights of the child's left and right subtree.
    pub heights: (u8, u8),
    /// The child's aggregate data.
    pub aggregate: StagedFeature,
}

impl StagedLink {
    fn of(link: &LinkPut<'_>) -> Self {
        Self {
            key: link.key.into(),
            hash: *link.hash,
            heights: link.heights,
            aggregate: link.aggregate.into(),
        }
    }

    fn as_put(&self) -> LinkPut<'_> {
        LinkPut {
            key: &self.key,
            hash: &self.hash,
            heights: self.heights,
            aggregate: &self.aggregate,
        }
    }
}

/// A node that a batch holds as the outcome of its key: every part of the
/// record that was put, but the Merks the node hosts, which the batch keeps
/// by the element ([`StagedBatch::host`](super::StagedBatch::host)).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagedNode {
    /// The node's key.
    pub key: StagedKey,
    /// The node's feature type with its aggregate data.
    pub feature: StagedFeature,
    /// The node's value hash.
    pub value_hash: Hash,
    /// The node's key-value hash.
    pub kv_hash: Hash,
    /// The link to the left child.
    pub left: Option<StagedLink>,
    /// The link to the right child.
    pub right: Option<StagedLink>,
    /// The node's value, or `None` where the node keeps the value the
    /// transaction has under its key when the batch is applied.
    pub value: Option<Box<[u8]>>,
    /// The hashes of the node that its writer did not make.
    pub stale: Stale,
    /// Where the writer last saw the node.
    pub at: Option<NodeHandle>,
}

impl StagedNode {
    pub(super) fn of(put: &NodePut<'_>) -> Self {
        Self {
            key: put.key.into(),
            feature: put.feature.into(),
            value_hash: *put.value_hash,
            kv_hash: *put.kv_hash,
            left: put.left.as_ref().map(StagedLink::of),
            right: put.right.as_ref().map(StagedLink::of),
            value: match put.value {
                ValuePut::Keep => None,
                ValuePut::Value(value) => Some(value.into()),
            },
            stale: put.stale,
            at: put.at,
        }
    }

    pub(super) fn as_put<'a>(&'a self, hosted: Hosted<'a>) -> NodePut<'a> {
        NodePut {
            key: &self.key,
            feature: &self.feature,
            value_hash: &self.value_hash,
            kv_hash: &self.kv_hash,
            left: self.left.as_ref().map(StagedLink::as_put),
            right: self.right.as_ref().map(StagedLink::as_put),
            hosted,
            value: match &self.value {
                None => ValuePut::Keep,
                Some(value) => ValuePut::Value(value),
            },
            stale: self.stale,
            at: self.at,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{hash_map::DefaultHasher, BTreeMap, HashSet};

    use super::*;

    fn hash_of(value: &impl StdHash) -> u64 {
        let mut hasher = DefaultHasher::new();
        value.hash(&mut hasher);
        hasher.finish()
    }

    #[test]
    fn bytes_are_kept_in_place_up_to_their_room_and_boxed_beyond_it() {
        for len in [0, 1, 45, 46, 47, 255, 256, 1000] {
            let bytes: Vec<u8> = (0..len).map(|at| at as u8).collect();
            let held = StagedKey::new(&bytes);
            assert_eq!(&*held, bytes.as_slice(), "{len} bytes");
            assert_eq!(held.is_inline(), len <= 46, "{len} bytes");
            assert_eq!(held.as_ref(), bytes.as_slice());
            assert_eq!(held.clone(), held);
            assert_eq!(StagedKey::from(bytes.as_slice()), held);
        }
        assert_eq!(size_of::<StagedKey>(), 48);
        assert_eq!(size_of::<StagedFeature>(), 32);
        // A length that one byte does not hold is boxed whatever the room.
        let long = [7; 300];
        let held = InlineBytes::<400>::new(&long);
        assert!(!held.is_inline());
        assert_eq!(&*held, &long[..]);
        assert!(InlineBytes::<400>::new(&long[..255]).is_inline());
    }

    #[test]
    fn bytes_are_compared_ordered_hashed_and_shown_as_their_slice() {
        let slices: [&[u8]; 7] = [
            b"",
            b"a",
            b"ab",
            b"b",
            &[b'b'; 46],
            &[b'b'; 47],
            &[b'c'; 100],
        ];
        for (at, one) in slices.iter().enumerate() {
            for (other_at, other) in slices.iter().enumerate() {
                let (held, other_held) = (StagedKey::new(one), StagedKey::new(other));
                assert_eq!(held == other_held, at == other_at);
                assert_eq!(held.cmp(&other_held), one.cmp(other));
                assert_eq!(held.partial_cmp(&other_held), Some(one.cmp(other)));
            }
            let held = StagedKey::new(one);
            assert_eq!(hash_of(&held), hash_of(one));
            assert_eq!(format!("{held:?}"), format!("{one:?}"));
        }
        // A map and a set that are keyed by the bytes are asked with a
        // slice, in place and boxed alike.
        let map: BTreeMap<StagedKey, usize> = (slices
            .iter()
            .enumerate()
            .map(|(at, bytes)| (StagedKey::new(bytes), at)))
        .collect();
        let set: HashSet<StagedKey> = slices.iter().map(|bytes| StagedKey::new(bytes)).collect();
        for (at, bytes) in slices.iter().enumerate() {
            assert_eq!(map.get(*bytes), Some(&at));
            assert!(set.contains(*bytes));
        }
        assert_eq!(map.get(&b"none"[..]), None);
        let in_order: Vec<&[u8]> = map.keys().map(|key| &**key).collect();
        assert_eq!(in_order, slices);
    }

    #[test]
    fn a_staged_node_is_the_put_it_was_made_of() {
        let (hash, other) = ([1; 32], [2; 32]);
        let link = |key: &'static [u8], aggregate: &'static [u8]| LinkPut {
            key,
            hash: &[3; 32],
            heights: (2, 1),
            aggregate,
        };
        let long_key = [9; 60];
        let put = NodePut {
            key: &long_key,
            feature: &[4, 0, 0, 0, 0, 0, 0, 0, 1],
            value_hash: &hash,
            kv_hash: &other,
            left: Some(link(b"left", &[])),
            right: Some(link(&[8; 47], &[1; 25])),
            hosted: Hosted::Keep,
            value: ValuePut::Value(b"value"),
            stale: Stale::NONE,
            at: None,
        };
        let staged = StagedNode::of(&put);
        assert!(!staged.key.is_inline() && staged.feature.is_inline());
        let (left, right) = (
            staged.left.as_ref().unwrap(),
            staged.right.as_ref().unwrap(),
        );
        assert!(left.key.is_inline() && left.aggregate.is_inline());
        assert!(!right.key.is_inline() && right.aggregate.is_inline());
        assert_eq!(staged.as_put(Hosted::Keep), put);

        let kept = NodePut {
            value: ValuePut::Keep,
            left: None,
            ..put
        };
        assert_eq!(StagedNode::of(&kept).as_put(Hosted::Keep), kept);
    }
}
