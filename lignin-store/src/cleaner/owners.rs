//! Two kinds of owners of values as one.

use lignin_format::{value_log::ValuePointer, PageId};

use crate::values::{OwnersError, PointerRewrite, Rewrite, ValueOwners};

/// The owners of values of two kinds of pages, as the one
/// [`ValueOwners`] that a cleaning of the value log is given.
///
/// A value's back pointer names a page and not what kind of page it is.
/// The layers above have one [`ValueOwners`] for the cells of tree pages
/// and one for the entries of flat leaves, and each finds nothing in a
/// page of the other kind. A cleaning that asked only one of them would
/// take the values of the other for dead and leave their file uncleaned.
/// This asks the first, and the second for what the first did not find.
#[derive(Debug, Clone, Copy)]
pub struct BothOwners<A, B>(pub A, pub B);

/// Which of the two owners of a [`BothOwners`] found the owner of a value,
/// with what it found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FoundBy<A, B> {
    /// The first.
    First(A),
    /// The second.
    Second(B),
}

impl<A: ValueOwners, B: ValueOwners> ValueOwners for BothOwners<A, B> {
    type Found = FoundBy<A::Found, B::Found>;

    fn find(
        &self,
        page: PageId,
        pointer: ValuePointer,
    ) -> Result<Option<Self::Found>, OwnersError> {
        if let Some(found) = self.0.find(page, pointer)? {
            return Ok(Some(FoundBy::First(found)));
        }
        Ok(self.1.find(page, pointer)?.map(FoundBy::Second))
    }

    fn rewrite(
        &self,
        step: PointerRewrite<'_>,
        found: Self::Found,
    ) -> Result<Rewrite, OwnersError> {
        match found {
            FoundBy::First(found) => self.0.rewrite(step, found),
            FoundBy::Second(found) => self.1.rewrite(step, found),
        }
    }
}
