//! Cleaning a value-log file: moving its live values to another file and
//! giving their owners the new pointers.

use lignin_format::{value_log::ValuePointer, CommitVersion, FileId, FormatVersion, PageId};

use super::{
    counts::ValueChange,
    error::{OwnersError, ValueStoreError},
    store::ValueStore,
    writer::{ValueHold, ValueWriter},
};
use crate::segment::ManifestStore;

/// The cells that own values, as the cleaner sees them.
///
/// The store knows no cell: the layers above it do. This is what they answer
/// for the cleaner. Both methods are about the page version that is current
/// when they are called, never about an older one.
pub trait ValueOwners {
    /// Whether the page `page` exists and one of its cells holds `pointer`.
    ///
    /// `true` proves the value is live, and that cell is its owner. `false`
    /// says only that the owner is not in this page: the value is dead, or
    /// its cell was carried to another page without it.
    fn holds(&self, page: PageId, pointer: ValuePointer) -> Result<bool, OwnersError>;

    /// Gives the cell of [`step.page()`](PointerRewrite::page) that holds
    /// [`step.from()`](PointerRewrite::from) the pointer
    /// [`step.to()`](PointerRewrite::to) instead, in one page step.
    ///
    /// The step is one step with respect to every commit that installs a
    /// version of that page and to the making of any private copy of it. In
    /// it, the implementation checks that:
    ///
    /// 1. the page version it changes is the current one
    /// 2. a cell of it still holds `from`
    /// 3. no open write transaction holds a private copy of the page
    ///
    /// If one fails, it installs nothing and returns [`Rewrite::Refused`]. If
    /// all hold, it stores the rewrite with the pages, so that a recovery
    /// which keeps it also applies [`step.change()`](PointerRewrite::change)
    /// to the counts, installs the new pointer, calls
    /// [`step.install`](PointerRewrite::install) inside the same step, and
    /// returns what that gave.
    fn rewrite(&self, step: PointerRewrite<'_>) -> Result<Rewrite, OwnersError>;
}

impl<O: ValueOwners + ?Sized> ValueOwners for &O {
    fn holds(&self, page: PageId, pointer: ValuePointer) -> Result<bool, OwnersError> {
        (**self).holds(page, pointer)
    }

    fn rewrite(&self, step: PointerRewrite<'_>) -> Result<Rewrite, OwnersError> {
        (**self).rewrite(step)
    }
}

/// One pointer rewrite, handed to [`ValueOwners::rewrite`].
#[derive(Debug)]
pub struct PointerRewrite<'a> {
    store: &'a ValueStore,
    page: PageId,
    from: ValuePointer,
    to: ValuePointer,
}

impl PointerRewrite<'_> {
    /// The page whose cell owns the value.
    pub fn page(&self) -> PageId {
        self.page
    }

    /// The pointer the cell holds now.
    pub fn from(&self) -> ValuePointer {
        self.from
    }

    /// The pointer the cell is to hold: the same value, in another file,
    /// with the same owner.
    pub fn to(&self) -> ValuePointer {
        self.to
    }

    /// What the rewrite does to the counts: one fewer live value in the file
    /// of [`Self::from`], one more in the file of [`Self::to`].
    pub fn change(&self) -> ValueChange {
        ValueChange::moved(self.from, self.to)
    }

    /// Counts the rewrite. Call it inside the page step that installs the
    /// new pointer, and nowhere else.
    ///
    /// `latest` is the latest commit version at that moment.
    pub fn install(self, latest: CommitVersion) -> Installed {
        let miscount = self.store.apply(&self.change(), latest).err();
        Installed { miscount }
    }
}

/// Proof that a pointer rewrite was counted, from
/// [`PointerRewrite::install`].
#[derive(Debug)]
pub struct Installed {
    miscount: Option<ValueStoreError>,
}

/// What became of a pointer rewrite.
#[derive(Debug)]
pub enum Rewrite {
    /// The cell holds the new pointer and the counts have changed.
    Installed(Installed),
    /// Nothing was installed and nothing was counted.
    Refused(Refusal),
}

/// Why a pointer rewrite was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// No cell of the current page version holds the old pointer any more:
    /// the value died, or was appended again, after the cleaner looked.
    OwnerGone,
    /// The page cannot take a rewrite now, for example because an open write
    /// transaction holds a private copy of it. The value is tried again
    /// later.
    Busy,
}

/// What one cleaning pass over a file did, from [`ValueStore::clean`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Cleaned {
    /// Values that were moved: their owners hold the new pointers.
    pub moved: u64,
    /// Values whose back pointer named a page that holds no cell with the
    /// pointer. Nothing was done with them.
    pub unowned: u64,
    /// Values that had an owner when the pass looked and none when the
    /// rewrite was to be installed ([`Refusal::OwnerGone`]).
    pub gone: u64,
    /// Values whose rewrite was refused for now ([`Refusal::Busy`]).
    pub busy: u64,
    /// The file's count of live values after the pass.
    pub live: u64,
}

impl Cleaned {
    /// Whether the file holds no live value any more.
    pub fn is_empty(&self) -> bool {
        self.live == 0
    }

    /// How many values the count calls live although the pass found no owner
    /// for them and was not refused one. A cell was carried to another page
    /// without its value, or a transaction that had appended the value ahead
    /// committed it while the pass ran. The file stays either way.
    pub fn owners_not_found(&self) -> u64 {
        self.live.saturating_sub(self.busy)
    }
}

struct Move {
    page: PageId,
    from: ValuePointer,
    to: ValuePointer,
}

impl ValueStore {
    /// Cleans the closed file `file`: moves each of its live values to the
    /// file of the writer `to`, and has its owner take the new pointer.
    ///
    /// 1. It walks the file. For each value it asks `owners` whether the page
    ///    the value's back pointer names holds the pointer to it
    ///    ([`ValueOwners::holds`]). If not, it goes on to the next value. If
    ///    so, it appends the value to `to` with the same back pointer.
    /// 2. It makes the copies durable and replaces the manifest, so that the
    ///    manifest covers every copy before a page points at one.
    /// 3. For each copy it asks `owners` to install the new pointer in one
    ///    page step ([`ValueOwners::rewrite`]). If the step is refused, the
    ///    copy is dead space and the value stays where it was.
    ///
    /// No value's bytes, length or owner change, and no value is removed. The
    /// file is deleted by [`Self::drop_empty`], and only once its count of
    /// live values is zero. A pass that finds no owner for a value the count
    /// includes leaves the file as it is ([`Cleaned::owners_not_found`]).
    ///
    /// If the pass fails part way, the rewrites installed so far stand, and
    /// the other copies are dead space. A rewrite that could not be counted
    /// as reported does not stop the pass: the pass is carried out in full
    /// and then fails with that [`ValueStoreError::Miscounted`].
    pub fn clean<O: ValueOwners + ?Sized>(
        &self,
        file: FileId,
        to: &mut ValueWriter,
        manifest: &mut ManifestStore,
        owners: &O,
    ) -> Result<Cleaned, ValueStoreError> {
        self.check_writable()?;
        self.check_same_dir(manifest)?;
        let source = self.file(file)?;
        if !source.is_closed() {
            return Err(ValueStoreError::NotClosed { file });
        }
        // A value and its owner have one encoding in a format version, so a
        // copy for the same owner is the value's records as they are. A
        // version with other records has to say here how they are copied.
        match source.version() {
            FormatVersion::V1 => {}
        }

        let mut cleaned = Cleaned::default();
        let mut hold = ValueHold::new();
        let mut moves = Vec::new();
        let mut walk = self.walk(file)?;
        while let Some(walked) = walk.next_walked()? {
            let (page, from) = (walked.value.owner(), walked.pointer);
            if !owners.holds(page, from).map_err(ValueStoreError::Owners)? {
                cleaned.unowned += 1;
                continue;
            }
            let to = to.append_records(&mut hold, from.value_len(), walked.records)?;
            moves.push(Move { page, from, to });
        }

        let mut miscount = None;
        if !moves.is_empty() {
            to.sync()?;
            manifest.update(|current| self.describe(current))?;
            self.name_files(manifest.manifest()?);
            for Move { page, from, to } in moves {
                let step = PointerRewrite {
                    store: self,
                    page,
                    from,
                    to,
                };
                match owners.rewrite(step).map_err(ValueStoreError::Owners)? {
                    Rewrite::Installed(installed) => {
                        cleaned.moved += 1;
                        miscount = miscount.or(installed.miscount);
                    }
                    Rewrite::Refused(Refusal::OwnerGone) => cleaned.gone += 1,
                    Rewrite::Refused(Refusal::Busy) => cleaned.busy += 1,
                }
            }
        }
        cleaned.live = self.cleaned(file, cleaned.busy == 0);
        drop(hold);
        miscount.map_or(Ok(cleaned), Err)
    }
}
