//! What an operation holds while it reads the store.

/// What an operation holds while it reads (`transactions.md` PN5): for as
/// long as it lives, every frame the operation reached, every page-table
/// entry it read and every file location it took from one stays valid.
///
/// Whoever keeps the guards implements this for its guard (TXN-02). The
/// operation takes its guard before it reads the latest version, or before
/// it asks at a version it has pinned.
///
/// The readers of committed pages borrow it, and what they hand out
/// borrows it too: [`NodeReader`](crate::node::NodeReader) for tree pages,
/// [`Committed`](crate::flat::Committed) for flat pages.
pub trait OperationGuard {}
