//! The keys an apply notes: a set of keys kept as their bytes in one buffer.

use std::{collections::BTreeSet, fmt};

/// The room the buffer of a set is given with its first key.
const FIRST_ROOM: usize = 2048;

/// The bytes a key's length takes in the buffer of a set.
const LEN: usize = size_of::<usize>();

/// A set of keys, kept as their bytes one after the other in one buffer.
///
/// An apply notes a key for every node it comes through, so the set of one
/// apply is filled key by key and asked once, by the commit. It answers
/// the questions a `BTreeSet<Vec<u8>>` of the keys answers.
#[derive(Clone, Default)]
pub struct KeySet {
    /// Each key behind its length, a `usize` in little-endian bytes. A
    /// key can be in it more than once.
    bytes: Vec<u8>,
}

impl KeySet {
    /// A set with no key.
    pub const fn new() -> Self {
        Self { bytes: Vec::new() }
    }

    /// Adds `key` and says whether the set did not hold it.
    pub fn insert(&mut self, key: impl AsRef<[u8]>) -> bool {
        let absent = !self.contains(key.as_ref());
        if absent {
            self.note(key.as_ref());
        }
        absent
    }

    /// Adds `key` without looking whether the set holds it.
    pub(crate) fn note(&mut self, key: &[u8]) {
        if self.bytes.capacity() == 0 {
            self.bytes
                .reserve(FIRST_ROOM.max(LEN.saturating_add(key.len())));
        }
        self.bytes.extend_from_slice(&key.len().to_le_bytes());
        self.bytes.extend_from_slice(key);
    }

    /// Whether the set holds `key`.
    pub fn contains(&self, key: &[u8]) -> bool {
        self.noted().any(|held| held == key)
    }

    /// How many keys the set holds.
    pub fn len(&self) -> usize {
        self.sorted().len()
    }

    /// Whether the set holds no key.
    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    /// The keys of the set, in ascending order.
    pub fn iter(&self) -> impl Iterator<Item = &[u8]> {
        self.sorted().into_iter()
    }

    /// Moves the keys of `other` into the set, and leaves `other` empty.
    pub fn append(&mut self, other: &mut Self) {
        if self.bytes.is_empty() {
            std::mem::swap(&mut self.bytes, &mut other.bytes);
        } else {
            self.bytes.append(&mut other.bytes);
        }
    }

    /// The keys as they were added.
    fn noted(&self) -> impl Iterator<Item = &[u8]> {
        let mut rest = self.bytes.as_slice();
        std::iter::from_fn(move || {
            let (len, after) = rest.split_first_chunk::<LEN>()?;
            let (key, after) = after.split_at_checked(usize::from_le_bytes(*len))?;
            rest = after;
            Some(key)
        })
    }

    fn sorted(&self) -> BTreeSet<&[u8]> {
        self.noted().collect()
    }
}

impl fmt::Debug for KeySet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_set().entries(self.sorted()).finish()
    }
}

impl PartialEq for KeySet {
    fn eq(&self, other: &Self) -> bool {
        self.sorted() == other.sorted()
    }
}

impl Eq for KeySet {}

impl PartialEq<BTreeSet<Vec<u8>>> for KeySet {
    fn eq(&self, other: &BTreeSet<Vec<u8>>) -> bool {
        self.sorted()
            .into_iter()
            .eq(other.iter().map(Vec::as_slice))
    }
}

impl From<BTreeSet<Vec<u8>>> for KeySet {
    fn from(keys: BTreeSet<Vec<u8>>) -> Self {
        keys.iter().collect()
    }
}

impl<K: AsRef<[u8]>> FromIterator<K> for KeySet {
    fn from_iter<I: IntoIterator<Item = K>>(keys: I) -> Self {
        let mut set = Self::new();
        for key in keys {
            set.note(key.as_ref());
        }
        set
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_set_answers_as_a_set_of_its_keys() {
        let mut set = KeySet::new();
        assert!(set.is_empty());
        assert_eq!(set.len(), 0);
        assert!(!set.contains(b""));

        assert!(set.insert(b"pear"));
        assert!(set.insert(vec![]));
        assert!(set.insert(b"apple"));
        assert!(!set.insert(b"pear"));
        assert!(!set.is_empty());
        assert_eq!(set.len(), 3);
        assert!(set.contains(b"pear") && set.contains(b"") && set.contains(b"apple"));
        assert!(!set.contains(b"pea") && !set.contains(b"pears"));
        let keys: Vec<&[u8]> = set.iter().collect();
        assert_eq!(keys, [&b""[..], b"apple", b"pear"]);
        assert_eq!(
            format!("{set:?}"),
            format!("{:?}", set.iter().collect::<BTreeSet<_>>())
        );
    }

    #[test]
    fn a_key_noted_twice_is_one_key() {
        let mut set = KeySet::new();
        set.note(b"k");
        set.note(b"k");
        set.note(&[7; 300]);
        assert_eq!(set.len(), 2);
        assert_eq!(set.iter().count(), 2);
        assert!(set.contains(&[7; 300]));
        assert_eq!(set, KeySet::from_iter([&[7u8; 300][..], b"k"]));
        assert_ne!(set, KeySet::from_iter([b"k"]));
    }

    #[test]
    fn a_set_is_compared_and_made_from_a_tree_set() {
        let tree_set = BTreeSet::from([vec![2], vec![1, 0], vec![]]);
        let set = KeySet::from(tree_set.clone());
        assert_eq!(set, tree_set);
        assert_ne!(set, BTreeSet::from([vec![2]]));
        assert_ne!(KeySet::new(), tree_set);
        assert_eq!(KeySet::new(), BTreeSet::new());
    }

    #[test]
    fn an_append_moves_every_key() {
        let mut first = KeySet::new();
        let mut second = KeySet::from_iter([b"b", b"c"]);
        first.append(&mut second);
        assert!(second.is_empty());
        assert_eq!(first.len(), 2);

        let mut third = KeySet::from_iter([b"a", b"b"]);
        first.append(&mut third);
        assert!(third.is_empty());
        assert_eq!(first, KeySet::from_iter([b"a", b"b", b"c"]));

        first.append(&mut KeySet::new());
        assert_eq!(first.len(), 3);
    }

    #[test]
    fn the_first_key_takes_the_room_of_many() {
        let mut set = KeySet::new();
        set.note(&[1; 32]);
        let room = set.bytes.capacity();
        assert!(room >= FIRST_ROOM);
        for key in 2..=50u8 {
            set.note(&[key; 32]);
        }
        assert_eq!(set.bytes.capacity(), room);

        let mut long = KeySet::new();
        long.note(&[0; 5000]);
        assert!(long.contains(&[0; 5000]));
    }
}
