//! The drop itself: what the transaction that drops or clears a Merk writes.

use std::collections::HashSet;

use lignin_format::CellAddress;

use super::{column::Flats, error::DropError};
use crate::{
    directory::{Directory, DirectoryError, Dropped, Entry, MerkId, Pending, Prefix},
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
/// root of each Merk named as dropped, the Merk's records in the directory,
/// and the flat records of each dropped Merk in every column.
///
/// `write` is the operation's write without those roots: the cell of each
/// host without its pointer, or among the removed ones, and what else the
/// operation changed. What it names as dropped already stays so. `flats`
/// is the flat side of the operation ([`Flats`]).
///
/// Nothing of the Merks is read but their entries, and nothing below them
/// at all: the cost does not depend on what they hold. From this call on no
/// lookup of `directory` returns a Merk below a dropped or cleared one.
///
/// **The flat records of a dropped Merk go here**, by the lazy range delete
/// ([`FlatDrops::unlink_prefix`](super::FlatDrops::unlink_prefix)): the
/// record that the directory keeps of a dropped Merk has no prefix, so no
/// reclaimer finds them later. A cleared Merk keeps its own flat records,
/// as it keeps its entry. Those of the Merks below either are the
/// reclaimer's, and no reader is shown them: they are found through an
/// entry, and no lookup returns the entry.
///
/// A Merk that is not entered, one that is named twice, one that is below
/// another of those named ([`DropError::Below`]: the outer one takes it
/// along), and one with no
/// root that has Merks below it are refused before any page is changed, and
/// so is a write that the tree pages refuse without a change
/// ([`FragmentError::Write`](crate::fragment::FragmentError::Write),
/// [`FragmentError::NoRoom`](crate::fragment::FragmentError::NoRoom)). After
/// any other error the caller rolls the operation's scope back.
///
/// **The directory's records are put into `pending`**, and no page of the
/// directory is changed: the caller settles them with the other records
/// of its batch ([`Directory::settle`]), before anything else reads the
/// directory. The Merks are read as the records that are pending already
/// leave them. The tree pages and the columns are written here.
///
/// **Every host inside a dropped Merk that is behind is written first**, by
/// a write of its own with its roots
/// ([`Pages::host_is_behind`], `tree-layout.md` T3): the reclaimer follows
/// every pointer it finds. Nothing here can see that this was left out. On
/// existing GroveVersions no host is behind.
#[allow(clippy::too_many_arguments)]
pub fn unlink<P: Pages, V: Values, F: FlatPagesMut + ?Sized>(
    fragments: &mut Fragments,
    pages: &mut P,
    values: &mut V,
    directory: &mut Directory,
    pending: &mut Pending,
    flats: &mut Flats<'_, F>,
    write: &Write<'_>,
    leaving: &[Leaving<'_>],
) -> Result<Unlink, DropError> {
    let mut dropped: Vec<CellAddress> = write.dropped.to_vec();
    let mut named = HashSet::new();
    let mut entries = Vec::with_capacity(leaving.len());
    for merk in leaving {
        let prefix = merk.prefix();
        if !named.insert(*prefix) {
            return Err(DirectoryError::Twice { prefix: *prefix }.into());
        }
        let entry = directory.open_in(pending, flats.pages, prefix)?;
        let entry = entry.ok_or(DirectoryError::NotFound { prefix: *prefix })?;
        match entry.root() {
            Some(root) => dropped.push(root.address),
            None => {
                let below = directory.first_child_in(pending, flats.pages, entry.id())?;
                if below.is_some() && matches!(merk, Leaving::Dropped(_)) {
                    return Err(DirectoryError::NotEmpty { prefix: *prefix }.into());
                }
            }
        }
        entries.push(entry);
    }
    // A Merk below another one that leaves goes with it: its cells would
    // be walked from both, and its entry is no entry once the outer one
    // has left.
    let ids: HashSet<MerkId> = entries.iter().map(Entry::id).collect();
    for (merk, entry) in leaving.iter().zip(&entries) {
        if entry.lineage().any(|above| ids.contains(&above)) {
            return Err(DropError::Below {
                prefix: *merk.prefix(),
            });
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
            Leaving::Dropped(prefix) => {
                left.push(directory.unlink_in(pending, flats.pages, prefix)?);
                flats.unlink_subtree(prefix)?;
            }
            Leaving::Cleared(prefix) => {
                left.extend(directory.clear_in(pending, flats.pages, prefix)?);
            }
        }
    }
    Ok(Unlink { report, left })
}
