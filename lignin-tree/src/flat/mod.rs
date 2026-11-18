//! The flat keyspace: a B+tree of flat pages for ordered data that is not
//! part of a Merk tree.

mod cursor;
mod error;
pub mod log;
mod long_key;
mod node;
mod owners;
mod pages;
mod store;
mod tree;

#[cfg(test)]
mod tests;

pub use cursor::Cursor;
pub use error::FlatError;
pub use owners::{FlatOwners, StepLog};
pub use pages::{FlatConfig, FlatPages, FlatPagesMut};
pub use store::{Committed, PageIds, Parts, Private, PrivatePage};
pub use tree::FlatTree;
