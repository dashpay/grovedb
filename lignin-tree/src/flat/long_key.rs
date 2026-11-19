//! Keys longer than a page stores: the bucket of a long key.
//!
//! A flat page holds a key whole (`flat-page.md`, section 6), so a tree
//! stores no key longer than [`FlatConfig::key_limit`]. A longer key is
//! kept in a **bucket**: one entry of the tree, whose stored key is the
//! first `key_limit` bytes of the long key followed by one zero byte, and
//! whose value is the root page of a tree of its own. That tree holds the
//! keys that begin with those bytes, each under what follows them (its
//! **rest**). A rest that is still too long has a bucket in that tree in
//! turn.
//!
//! A stored key that is longer than the limit is therefore a bucket, and no
//! key of a caller is ever stored at that length. A bucket sorts where its
//! keys belong: behind the key that is exactly its head, if there is one,
//! and before every other key that is greater than the head. A write never
//! leaves its tree empty: the last key that a delete takes out of it takes
//! the bucket along.

use lignin_format::{flat_page::FlatValue, PageId};

use super::{error::FlatError, pages::FlatConfig};

/// How a tree stores a key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Stored<'a> {
    /// As it is.
    Short(&'a [u8]),
    /// In the tree of the bucket with this stored key, under `rest`.
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

    /// Whether an entry of a tree with a stored key of `len` bytes is a
    /// bucket.
    pub(super) const fn is_bucket(&self, len: usize) -> bool {
        len > self.key_limit()
    }
}

/// The root of the tree that the bucket in the leaf `page` holds, read
/// from the bucket's value.
pub(super) fn bucket_root(page: PageId, value: FlatValue<'_>) -> Result<PageId, FlatError> {
    let root = match value {
        FlatValue::Inline(bytes) => bytes.try_into().ok().and_then(PageId::decode),
        FlatValue::OutOfLine(_) => None,
    };
    root.ok_or(FlatError::Damaged {
        page,
        what: "a bucket of long keys that names no page",
    })
}
