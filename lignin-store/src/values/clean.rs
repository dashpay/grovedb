//! Cleaning a value-log file: moving its live values to another file and
//! giving their owners the new pointers.

use std::sync::atomic::{AtomicBool, Ordering};

use lignin_format::{value_log::ValuePointer, CommitVersion, FileId, FormatVersion, PageId};

use super::{
    counts::ValueChange,
    error::{Miscount, OwnersError, ValueStoreError},
    store::ValueStore,
    walk::ValueWalk,
    writer::{ValueHold, ValueWriter},
};
use crate::segment::ManifestStore;

/// The cells that own values, as the cleaner sees them.
///
/// The store knows no cell: the layers above it do. This is what they answer
/// for the cleaner. Both methods are about the page version that is current
/// when they are called, never about an older one.
pub trait ValueOwners {
    /// What a look at a page found: whatever the implementation needs to
    /// tell, in the page step, whether the page is still as it was looked
    /// at. The version of the page that was current is enough.
    type Found;

    /// Looks in the page `page` for a cell that holds `pointer`.
    ///
    /// `Some` proves the value is live, and that cell is its owner. `None`
    /// says only that the owner is not in this page: the page does not
    /// exist, the value is dead, or its cell was carried to another page
    /// without it.
    fn find(&self, page: PageId, pointer: ValuePointer)
        -> Result<Option<Self::Found>, OwnersError>;

    /// Gives the cell of [`step.page()`](PointerRewrite::page) that holds
    /// [`step.from()`](PointerRewrite::from) the pointer
    /// [`step.to()`](PointerRewrite::to) instead, in one page step. `found`
    /// is what [`Self::find`] gave for that cell.
    ///
    /// The step is one step with respect to every commit that installs a
    /// version of that page and to the making of any private copy of it. In
    /// it, the implementation checks that:
    ///
    /// 1. the page version it changes is the current one, and is the one
    ///    that was looked at: no commit has superseded the page since
    /// 2. a cell of it still holds `from`
    /// 3. no open write transaction holds a private copy of the page
    ///
    /// If one fails, it installs nothing and returns [`Rewrite::Refused`]. If
    /// all hold, it stores the rewrite with the pages, so that a recovery
    /// which keeps it also applies [`step.change()`](PointerRewrite::change)
    /// to the counts, installs the new pointer, calls
    /// [`step.install`](PointerRewrite::install) inside the same step, and
    /// returns what that gave.
    fn rewrite(&self, step: PointerRewrite<'_>, found: Self::Found)
        -> Result<Rewrite, OwnersError>;
}

impl<O: ValueOwners + ?Sized> ValueOwners for &O {
    type Found = O::Found;

    fn find(
        &self,
        page: PageId,
        pointer: ValuePointer,
    ) -> Result<Option<Self::Found>, OwnersError> {
        (**self).find(page, pointer)
    }

    fn rewrite(
        &self,
        step: PointerRewrite<'_>,
        found: Self::Found,
    ) -> Result<Rewrite, OwnersError> {
        (**self).rewrite(step, found)
    }
}

/// One pointer rewrite, handed to [`ValueOwners::rewrite`].
#[derive(Debug)]
pub struct PointerRewrite<'a> {
    store: &'a ValueStore,
    counted: &'a AtomicBool,
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
    ///
    /// The value this returns is the only way to report the rewrite
    /// installed. If it was called and [`ValueOwners::rewrite`] then returns
    /// anything else, the store no longer knows which of the two pointers
    /// the cell holds, and keeps the file of the old one for good.
    pub fn install(self, latest: CommitVersion) -> Installed {
        self.counted.store(true, Ordering::Release);
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
    /// The page cannot take a rewrite now: a commit has superseded it since
    /// the cleaner looked, or an open write transaction holds a private copy
    /// of it. The value is tried again later.
    Busy,
}

/// What one cleaning pass over a file did, from [`ValueStore::install`] or
/// [`ValueStore::clean`].
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
    /// The bytes the pass appended to the other file: the records of every
    /// value it copied, whether its rewrite was installed or not.
    pub copied_bytes: u64,
}

impl Cleaned {
    /// Whether the file holds no live value any more.
    pub fn is_empty(&self) -> bool {
        self.live == 0
    }

    /// How many values the count calls live although the pass found no owner
    /// for them and was not refused one. A cell was carried to another page
    /// without its value, which breaks the move rule, or a transaction that
    /// had appended the value ahead committed it while the pass ran. The two
    /// look the same from here, so this is a number and not an error. The
    /// file stays either way.
    pub fn owners_not_found(&self) -> u64 {
        self.live.saturating_sub(self.busy)
    }
}

#[derive(Debug)]
struct Move<F> {
    page: PageId,
    from: ValuePointer,
    to: ValuePointer,
    found: F,
}

/// The live values of one file, copied to another file and not yet owned
/// there: the first half of a cleaning pass, from [`ValueStore::copy_live`].
///
/// It holds the file the copies are in. Dropping it gives the copies up:
/// they are dead space.
///
/// `F` is what the owners' look found for each value
/// ([`ValueOwners::Found`]).
#[derive(Debug)]
pub struct CopiedValues<F> {
    file: FileId,
    session: u64,
    began: u64,
    moves: Vec<Move<F>>,
    unowned: u64,
    copied_bytes: u64,
    hold: ValueHold,
}

impl<F> CopiedValues<F> {
    /// The file that is being cleaned.
    pub fn file(&self) -> FileId {
        self.file
    }

    /// How many values were copied.
    pub fn len(&self) -> usize {
        self.moves.len()
    }

    /// Whether no value was copied, so that nothing has to be made durable
    /// before [`ValueStore::install`].
    pub fn is_empty(&self) -> bool {
        self.moves.is_empty()
    }
}

impl ValueStore {
    /// The first half of a cleaning pass over the closed file `file`: copies
    /// each of its live values to the file of the writer `to`.
    ///
    /// It walks the file. For each value it asks `owners` whether the page
    /// the value's back pointer names holds the pointer to it
    /// ([`ValueOwners::find`]). If not, it goes on to the next value. If
    /// so, it appends the value to `to` with the same back pointer.
    ///
    /// `to` must be a writer of this store
    /// ([`ValueStoreError::WrongStore`]).
    ///
    /// Nothing points at the copies yet. Before [`Self::install`], make them
    /// durable and have the manifest cover them: sync `to` and replace the
    /// manifest ([`Self::commit`]). [`Self::clean`] does all three.
    ///
    /// If this fails part way, the copies made so far are dead space.
    pub fn copy_live<O: ValueOwners + ?Sized>(
        &self,
        file: FileId,
        to: &mut ValueWriter,
        owners: &O,
    ) -> Result<CopiedValues<O::Found>, ValueStoreError> {
        self.check_writable()?;
        self.check_own(to.session())?;
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

        let mut copies = CopiedValues {
            file,
            session: self.session(),
            began: self.applied_so_far(),
            moves: Vec::new(),
            unowned: 0,
            copied_bytes: 0,
            hold: ValueHold::new(),
        };
        let mut walk = ValueWalk::new(source);
        while let Some(walked) = walk.next_walked()? {
            let (page, from) = (walked.value.owner(), walked.pointer);
            let Some(found) = owners.find(page, from).map_err(ValueStoreError::Owners)? else {
                copies.unowned += 1;
                continue;
            };
            let to = to.append_records(&mut copies.hold, from.value_len(), walked.records)?;
            copies.copied_bytes += walked.records.len() as u64;
            copies.moves.push(Move {
                page,
                from,
                to,
                found,
            });
        }
        Ok(copies)
    }

    /// The second half of a cleaning pass: has the owner of each copied
    /// value take the pointer to the copy.
    ///
    /// For each copy it asks `owners` to install the new pointer in one page
    /// step ([`ValueOwners::rewrite`]). If the step is refused, the copy is
    /// dead space and the value stays where it was.
    ///
    /// Fails with [`ValueStoreError::NotCovered`], and installs nothing, if
    /// the durable manifest does not cover every copy: a page must not point
    /// at a value that a crash could take away. Fails with
    /// [`ValueStoreError::WrongStore`] for copies another store made.
    ///
    /// If it fails part way, the rewrites installed so far stand, and the
    /// other copies are dead space. A rewrite that could not be counted as
    /// reported does not stop the pass: the pass is carried out as far as it
    /// goes and then fails with that [`ValueStoreError::Miscounted`], before
    /// any other error.
    pub fn install<O: ValueOwners + ?Sized>(
        &self,
        copies: CopiedValues<O::Found>,
        owners: &O,
    ) -> Result<Cleaned, ValueStoreError> {
        self.check_writable()?;
        self.check_own(copies.session)?;
        if let Some(uncovered) = copies.moves.iter().find(|copy| !self.covers(copy.to)) {
            return Err(ValueStoreError::NotCovered {
                pointer: uncovered.to,
            });
        }
        let mut cleaned = Cleaned {
            unowned: copies.unowned,
            copied_bytes: copies.copied_bytes,
            ..Cleaned::default()
        };
        let mut miscount = None;
        let mut failed = None;
        let CopiedValues {
            file,
            began,
            moves,
            hold,
            ..
        } = copies;
        for Move {
            page,
            from,
            to,
            found,
        } in moves
        {
            let counted = AtomicBool::new(false);
            let step = PointerRewrite {
                store: self,
                counted: &counted,
                page,
                from,
                to,
            };
            let outcome = owners.rewrite(step, found);
            if counted.into_inner() && !matches!(outcome, Ok(Rewrite::Installed(_))) {
                self.mark_miscounted(from.file());
                miscount.get_or_insert(ValueStoreError::Miscounted {
                    pointer: from,
                    why: Miscount::NotInstalled,
                });
            }
            match outcome {
                Ok(Rewrite::Installed(installed)) => {
                    cleaned.moved += 1;
                    miscount = miscount.or(installed.miscount);
                }
                Ok(Rewrite::Refused(Refusal::OwnerGone)) => cleaned.gone += 1,
                Ok(Rewrite::Refused(Refusal::Busy)) => cleaned.busy += 1,
                Err(error) => {
                    failed = Some(ValueStoreError::Owners(error));
                    break;
                }
            }
        }
        let refused = cleaned.busy != 0 || failed.is_some();
        cleaned.live = self.end_pass(file, began, refused);
        drop(hold);
        miscount.or(failed).map_or(Ok(cleaned), Err)
    }

    /// Cleans the closed file `file`: moves each of its live values to the
    /// file of the writer `to`, and has its owner take the new pointer.
    ///
    /// 1. [`Self::copy_live`] copies the values whose owners it finds.
    /// 2. The copies are made durable and the manifest is replaced, so that
    ///    it covers every copy before a page points at one.
    /// 3. [`Self::install`] has each owner take the new pointer.
    ///
    /// No value's bytes, length or owner change, and no value is removed. The
    /// file is deleted by [`Self::drop_empty`], and only once its count of
    /// live values is zero. A pass that finds no owner for a value the count
    /// includes leaves the file as it is ([`Cleaned::owners_not_found`]).
    ///
    /// The manifest is needed for step 2 alone. A caller that must not keep
    /// it from others for the length of a pass makes the three steps itself.
    ///
    /// The manifest this writes may stop naming a file that
    /// [`Self::drop_empty`] took out. That file is deleted by the next
    /// [`Self::manifest_replaced`], not here.
    pub fn clean<O: ValueOwners + ?Sized>(
        &self,
        file: FileId,
        to: &mut ValueWriter,
        manifest: &mut ManifestStore,
        owners: &O,
    ) -> Result<Cleaned, ValueStoreError> {
        self.check_same_dir(manifest)?;
        let copies = self.copy_live(file, to, owners)?;
        if !copies.is_empty() {
            to.sync()?;
            manifest.update(|current| self.describe(current))?;
            self.name_files(manifest.manifest()?);
        }
        self.install(copies, owners)
    }
}
