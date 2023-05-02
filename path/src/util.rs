//! Utilities module for path library.

use std::ops::Deref;

/// A smart pointer that follows the semantics of [Cow](std::borrow::Cow) except
/// provides no means for mutability and thus doesn't require [Clone].
#[derive(Debug)]
pub(crate) enum CowLike<'b> {
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
