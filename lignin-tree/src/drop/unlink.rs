//! The drop itself: what the transaction that drops or clears a Merk writes.

use std::collections::HashSet;

use lignin_format::CellAddress;

use super::error::DropError;
use crate::{
    directory::{Directory, DirectoryError, Dropped, Prefix},
    flat::FlatPagesMut,
    fragment::{Fragments, Pages, Report, Values, Write},
};

/// A Merk that an operation takes out of the grove.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Leaving<'a> {
    /// The Merk under the prefix is dropped: its host loses the pointer to
    /// it, or is deleted, and its directory entry goes
    /// (`tree-layout.md` D1).
    Dropped(&'a Prefix),
    /// The Merk under the prefix is cleared: it stays, with no node
    /// (`tree-layout.md` D4).
    Cleared(&'a Prefix),
}

impl<'a> Leaving<'a> {
    /// The prefix of the Merk.
    pub const fn prefix(&self) -> &'a Prefix {
        match self {
            Self::Dropped(prefix) | Self::Cleared(prefix) => prefix,
        }
    }
}

/// What [`unlink`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unlink {
    /// The report of the write to the tree pages: the caller states the
    /// roots it changed to the directory from it, as after any write.
    pub report: Report,
    /// What each Merk left for the reclaimer, in the order they were
    /// given. A cleared Merk that had no node left nothing, and is not
    /// among them.
    pub left: Vec<Dropped>,
}

/// Applies the write of an operation that drops or clears Merks, and
/// unlinks them: the one write of the operation to the tree pages, with the
/// root of each Merk named as dropped, and the Merk's records in the
/// directory.
///
/// `write` is the operation's write without those roots: the cell of each
/// host without its pointer, or among the removed ones, and what else the
/// operation changed. What it names as dropped already stays so.
///
/// Nothing of the Merks is read but their entries, and nothing below them
/// at all: the cost does not depend on what they hold. From this call on no
/// lookup of `directory` returns a Merk below a dropped or cleared one.
///
/// A Merk that is not entered, one that is named twice, and one with no
/// root that has Merks below it are refused before any page is changed, and
/// so is a write that the tree pages refuse without a change
/// ([`FragmentError::Write`](crate::fragment::FragmentError::Write),
/// [`FragmentError::NoRoom`](crate::fragment::FragmentError::NoRoom)). After
/// any other error the caller rolls the operation's scope back.
///
/// **Every host inside a dropped Merk that is behind is written first**, by
/// a write of its own with its roots
/// ([`Pages::host_is_behind`], `tree-layout.md` T3): the reclaimer follows
/// every pointer it finds. Nothing here can see that this was left out. On
/// existing GroveVersions no host is behind.
pub fn unlink<P: Pages, V: Values, F: FlatPagesMut + ?Sized>(
    fragments: &mut Fragments,
    pages: &mut P,
    values: &mut V,
    directory: &mut Directory,
    flat: &mut F,
    write: &Write<'_>,
    leaving: &[Leaving<'_>],
) -> Result<Unlink, DropError> {
    let mut dropped: Vec<CellAddress> = write.dropped.to_vec();
    let mut named = HashSet::new();
    for merk in leaving {
        let prefix = merk.prefix();
        if !named.insert(*prefix) {
            return Err(DirectoryError::Twice { prefix: *prefix }.into());
        }
        let entry = directory.open(flat, prefix)?;
        let entry = entry.ok_or(DirectoryError::NotFound { prefix: *prefix })?;
        match entry.root() {
            Some(root) => dropped.push(root.address),
            None => {
                let below = directory.children(entry.id()).next(flat)?;
                if below.is_some() && matches!(merk, Leaving::Dropped(_)) {
                    return Err(DirectoryError::NotEmpty { prefix: *prefix }.into());
                }
            }
        }
    }

    let write = Write {
        dropped: &dropped,
        ..*write
    };
    let report = fragments.apply(pages, values, &write)?;
    let mut left = Vec::with_capacity(leaving.len());
    for merk in leaving {
        match merk {
            Leaving::Dropped(prefix) => left.push(directory.unlink(flat, prefix)?),
            Leaving::Cleared(prefix) => left.extend(directory.clear(flat, prefix)?),
        }
    }
    Ok(Unlink { report, left })
}
