//! Keys longer than a page stores: the bucket of a long key.
//!
//! A flat page holds a key whole (`flat-page.md`, section 6), so the tree
//! stores no key longer than [`FlatConfig::key_limit`]. A longer key is kept
//! in a **bucket**: one entry of the tree, whose stored key is the first
//! `key_limit` bytes of the long key followed by one zero byte, and whose
//! value lists the keys that begin with those bytes, each as what follows
//! them (its **rest**) and its value, in the order of the rests.
//!
//! A stored key that is longer than the limit is therefore a bucket, and no
//! key of a caller is ever stored at that length. A bucket sorts where its
//! keys belong: behind the key that is exactly its head, if there is one,
//! and before every other key that is greater than the head.

use super::{error::FlatError, pages::FlatConfig};

/// How the tree stores a key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Stored<'a> {
    /// As it is.
    Short(&'a [u8]),
    /// In the bucket with this stored key, under `rest`.
    Long { bucket: Vec<u8>, rest: &'a [u8] },
}

impl FlatConfig {
    pub(super) fn stored<'a>(&self, key: &'a [u8]) -> Stored<'a> {
        match key.split_at_checked(self.key_limit()) {
            Some((head, rest)) if !rest.is_empty() => {
                let mut bucket = Vec::with_capacity(head.len() + 1);
                bucket.extend_from_slice(head);
                bucket.push(0);
                Stored::Long { bucket, rest }
            }
            _ => Stored::Short(key),
        }
    }

    /// Whether `stored`, a key of an entry of the tree, is a bucket's.
    pub(super) fn is_bucket(&self, stored: &[u8]) -> bool {
        stored.len() > self.key_limit()
    }
}

/// The keys of one bucket, each as its rest and its value, in the order of
/// the rests.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct Bucket {
    records: Vec<(Vec<u8>, Vec<u8>)>,
}

const LEN: usize = size_of::<u32>();

impl Bucket {
    /// Reads the value of a bucket's entry, or `None` if the bytes are not
    /// one.
    pub(super) fn decode(mut bytes: &[u8]) -> Option<Self> {
        let mut records: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();
        let field = |bytes: &mut &[u8]| {
            let (len, rest) = bytes.split_first_chunk::<LEN>()?;
            let (field, rest) = rest.split_at_checked(u32::from_le_bytes(*len) as usize)?;
            *bytes = rest;
            Some(field.to_vec())
        };
        while !bytes.is_empty() {
            let rest = field(&mut bytes)?;
            let value = field(&mut bytes)?;
            if rest.is_empty() || records.last().is_some_and(|(last, _)| *last >= rest) {
                return None;
            }
            records.push((rest, value));
        }
        (!records.is_empty()).then_some(Self { records })
    }

    /// The value of the bucket's entry.
    pub(super) fn encode(&self) -> Result<Vec<u8>, FlatError> {
        let mut out = Vec::new();
        for field in self.records.iter().flat_map(|(rest, value)| [rest, value]) {
            let len = u32::try_from(field.len())
                .map_err(|_| FlatError::ValueTooLong { len: field.len() })?;
            out.extend_from_slice(&len.to_le_bytes());
            out.extend_from_slice(field);
        }
        Ok(out)
    }

    pub(super) fn len(&self) -> usize {
        self.records.len()
    }

    pub(super) fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    /// The position of `rest`, or the position at which it would stand.
    pub(super) fn search(&self, rest: &[u8]) -> Result<usize, usize> {
        self.records
            .binary_search_by(|(found, _)| found.as_slice().cmp(rest))
    }

    pub(super) fn rest(&self, at: usize) -> Option<&[u8]> {
        self.records.get(at).map(|(rest, _)| rest.as_slice())
    }

    pub(super) fn value(&self, at: usize) -> Option<&[u8]> {
        self.records.get(at).map(|(_, value)| value.as_slice())
    }

    pub(super) fn get(&self, rest: &[u8]) -> Option<&[u8]> {
        self.value(self.search(rest).ok()?)
    }

    pub(super) fn put(&mut self, rest: &[u8], value: &[u8]) {
        match self.search(rest) {
            Ok(at) => self.records[at].1 = value.to_vec(),
            Err(at) => self.records.insert(at, (rest.to_vec(), value.to_vec())),
        }
    }

    /// Takes `rest` out, and says whether it was there.
    pub(super) fn remove(&mut self, rest: &[u8]) -> bool {
        self.search(rest)
            .map(|at| {
                self.records.remove(at);
            })
            .is_ok()
    }

    /// Takes out every rest from `from` up to, and not including, `to`, and
    /// says whether there was one.
    pub(super) fn remove_range(&mut self, from: Option<&[u8]>, to: Option<&[u8]>) -> bool {
        let before = self.records.len();
        self.records.retain(|(rest, _)| {
            from.is_some_and(|from| rest.as_slice() < from)
                || to.is_some_and(|to| rest.as_slice() >= to)
        });
        self.records.len() != before
    }
}
