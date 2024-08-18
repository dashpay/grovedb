//! Module for [CowLikeMerk]: simple abstraction over owned and borrowed Merk.

use std::{
    hash::{Hash, Hasher},
    ops::Deref,
};
use grovedb_storage::StorageContext;
use crate::Merk;

/// A smart pointer that follows the semantics of [Cow](std::borrow::Cow) except
/// provides no means for mutability and thus doesn't require [Clone].
#[derive(Debug)]
pub enum CowLikeMerk<'db, S> {
    /// Owned variant
    Owned(Merk<S>),
    /// Borrowed variant
    Borrowed(&'db Merk<S>),
}

impl<'db, S: StorageContext<'db>> Deref for CowLikeMerk<'db, S> {
    type Target = Merk<S>;

    fn deref(&self) -> &Self::Target {
        match self {
            Self::Owned(v) => v,
            Self::Borrowed(s) => s,
        }
    }
}