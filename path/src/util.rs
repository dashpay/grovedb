//! Utilities module for path library.
mod bytes_2d;
mod cow_like;

#[cfg(test)]
use std::hash::{Hash, Hasher};

pub(crate) use cow_like::CowLike;

#[cfg(test)]
pub(crate) fn calculate_hash<T: Hash>(t: &T) -> u64 {
    use std::collections::hash_map::DefaultHasher;

    let mut s = DefaultHasher::new();
    t.hash(&mut s);
    s.finish()
}
