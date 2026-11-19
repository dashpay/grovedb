//! The pages of a flat keyspace in the page store: committed pages through
//! the loader, and the private copies of one writer in the buffer pool.

use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    fmt,
    ops::Deref,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
};

use lignin_format::{
    flat_page::FlatValue,
    value_log::{PendingValue, ValuePlace, ValuePointer},
    wal::GroupWriter,
    CommitVersion, PageId,
};
use lignin_store::{
    alloc::{AllocError, PageAllocator},
    cache::{
        BufferPool, CacheError, ExclusiveLatch, NotMade, OwnerId, PageRef, PrivateRef, SharedLatch,
        ValueId,
    },
    load::{LoadBuffers, PageLoader},
    values::ValueStore,
};

use super::{
    error::FlatError,
    log::{is_changed, push_changes, stamp_pending},
    node::{place, view},
    pages::{value_len, FlatPages, FlatPagesMut, Revision},
};
use crate::OperationGuard;

/// The pending values of the leaf at the start of `bytes`, each with the
/// position of its entry. A branch has none.
fn pending(bytes: &[u8]) -> Result<Vec<(usize, PendingValue)>, FlatError> {
    let Some(leaf) = view(bytes)?.leaf() else {
        return Ok(Vec::new());
    };
    let mut found = Vec::new();
    for (slot, (_, value)) in leaf.iter().enumerate() {
        if let FlatValue::OutOfLine(bytes) = value
            && let ValuePlace::Pending(pending) = place(bytes)?
        {
            found.push((slot, pending));
        }
    }
    Ok(found)
}

/// The committed pages of a store, as a reader at one version sees them.
///
/// A page comes from the buffer pool, or is loaded into it
/// (`lignin_store::load`). A value in the value log is read through its
/// pointer, with the page as its owner. A pending value is read from the
/// frame of its page, where its bytes are attached under the eight bytes
/// that name it (`transactions.md` FL7, `log-format.md` LG21).
///
/// It borrows the guard of the reader's operation (PN5), and a page it
/// hands out borrows it too: what was read here does not outlive the guard
/// it was read under. The reader reads at a version it has pinned, or at
/// the latest version, read after the guard was taken.
///
/// ```compile_fail,E0505
/// # use lignin_format::{CommitVersion, PageId};
/// # use lignin_store::{load::PageLoader, values::ValueStore};
/// # use lignin_tree::{flat::{Committed, FlatPages}, OperationGuard};
/// # struct Guard;
/// # impl OperationGuard for Guard {}
/// # fn read(loader: PageLoader<'_>, values: &ValueStore, at: CommitVersion, page: PageId) {
/// let guard = Guard;
/// let pages = Committed::new(loader, values, &guard, at);
/// let held = pages.page(page);
/// drop(guard);
/// drop(held);
/// # }
/// ```
pub struct Committed<'s, 'r, G: ?Sized> {
    loader: PageLoader<'s>,
    values: &'s ValueStore,
    guard: &'r G,
    at: CommitVersion,
    buffers: RefCell<(LoadBuffers, Vec<u8>)>,
}

impl<'s: 'r, 'r, G: OperationGuard + ?Sized> Committed<'s, 'r, G> {
    /// The pages of `loader`'s store at version `at`, with their values out
    /// of line in `values`, for the operation that holds `guard`.
    ///
    /// `at` is a version the caller has pinned, or the latest version, read
    /// after `guard` was taken.
    pub fn new(
        loader: PageLoader<'s>,
        values: &'s ValueStore,
        guard: &'r G,
        at: CommitVersion,
    ) -> Self {
        Self {
            loader,
            values,
            guard,
            at,
            buffers: RefCell::default(),
        }
    }

    /// The version the pages are read at.
    pub const fn at(&self) -> CommitVersion {
        self.at
    }

    /// The guard of the reader's operation.
    pub const fn guard(&self) -> &'r G {
        self.guard
    }

    /// The same pages at the same version, for the operation that holds
    /// `guard`: a reader that goes on in another operation.
    ///
    /// The version is one the caller has pinned through both operations.
    /// A reader with no pin does not go on this way: the latest version it
    /// read under one guard can be reclaimed once that guard is dropped,
    /// whatever guard is held then (PN5, PN6), so it reads the latest
    /// version again under `guard` and begins with [`Self::new`].
    pub fn under<'q, H: OperationGuard + ?Sized>(self, guard: &'q H) -> Committed<'s, 'q, H>
    where
        's: 'q,
    {
        Committed {
            loader: self.loader,
            values: self.values,
            guard,
            at: self.at,
            buffers: self.buffers,
        }
    }

    /// Appends the value at `pointer`, which an entry of `page` holds, to
    /// `out`.
    fn read_placed(
        &self,
        pointer: ValuePointer,
        page: PageId,
        out: &mut Vec<u8>,
    ) -> Result<(), FlatError> {
        let buf = &mut self.buffers.borrow_mut().1;
        let value = self.values.read(pointer, page, buf)?;
        value.parts().for_each(|part| out.extend_from_slice(part));
        Ok(())
    }
}

impl<G: ?Sized> fmt::Debug for Committed<'_, '_, G> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Committed")
            .field("at", &self.at)
            .finish_non_exhaustive()
    }
}

/// Appends the bytes of the pending value `pending`, which an entry of the
/// page in `frame` holds, to `out`: they are attached to the frame under
/// the value's name.
fn read_attached(
    frame: &PageRef<'_>,
    pending: PendingValue,
    out: &mut Vec<u8>,
) -> Result<(), FlatError> {
    let bytes = frame
        .attached(pending.id())
        .filter(|bytes| bytes.len() == pending.value_len() as usize)
        .ok_or(FlatError::NoValue { page: frame.page() })?;
    out.extend_from_slice(bytes);
    Ok(())
}

impl<'s: 'r, 'r, G: OperationGuard + ?Sized> FlatPages for Committed<'s, 'r, G> {
    type Page<'a>
        = PageRef<'r>
    where
        Self: 'a;

    fn page(&self, page: PageId) -> Result<PageRef<'r>, FlatError> {
        let buffers = &mut self.buffers.borrow_mut().0;
        Ok(self.loader.get(page, self.at, buffers)?)
    }

    fn read_value(
        &self,
        held: &PageRef<'r>,
        page: PageId,
        _slot: usize,
        place: ValuePlace,
        out: &mut Vec<u8>,
    ) -> Result<(), FlatError> {
        match place {
            ValuePlace::Placed(pointer) => self.read_placed(pointer, page, out),
            ValuePlace::Pending(pending) => read_attached(held, pending, out),
        }
    }

    /// The version the pages are read at: readers of one version read the
    /// same pages.
    fn revision(&self) -> Revision {
        Revision::Committed(self.at)
    }
}

/// Where a writer gets the ids of its new pages, and gives back those it
/// did not keep (`transactions.md` ID1).
pub trait PageIds {
    /// An id that nothing else has seen.
    fn allocate(&mut self) -> Result<PageId, AllocError>;

    /// The id of a page that the writer made and freed again.
    fn release(&mut self, page: PageId) -> Result<(), AllocError>;
}

impl PageIds for &PageAllocator {
    fn allocate(&mut self) -> Result<PageId, AllocError> {
        PageAllocator::allocate(self)
    }

    fn release(&mut self, page: PageId) -> Result<(), AllocError> {
        PageAllocator::release(self, page)
    }
}

/// A value that an entry of a private copy holds as pending.
#[derive(Debug)]
struct Held {
    bytes: Arc<[u8]>,
    /// Whether the value was pending in the page when the copy was made:
    /// the log group of an earlier commit holds it. Every other value is
    /// the writer's, and goes into the group of the writer's commit.
    inherited: bool,
}

/// One private copy of a writer.
#[derive(Debug)]
struct Own<'s> {
    frame: PrivateRef<'s>,
    /// Whether the writer made the page.
    created: bool,
    /// The pending values of the page, by the 8 bytes that name them.
    values: HashMap<ValueId, Held>,
}

/// A page as a writer reads it: its own copy, or the committed page.
#[derive(Debug)]
pub enum PrivatePage<'a, 'r> {
    /// A committed page that the writer has not copied.
    Committed(PageRef<'r>),
    /// The writer's copy, under its shared latch.
    Own(SharedLatch<'a>),
}

impl Deref for PrivatePage<'_, '_> {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        match self {
            Self::Committed(page) => page,
            Self::Own(latch) => latch,
        }
    }
}

/// What a writer leaves for its commit: see [`Private::into_parts`].
#[derive(Debug)]
pub struct Parts<'s> {
    /// The copies of the pages the writer took or made, in ascending order
    /// of page id, each with whether the writer made the page. A copy of a
    /// page that is as it was has no run in the log ([`Private::log_page`])
    /// and is discarded, not installed.
    pub written: Vec<(PrivateRef<'s>, bool)>,
    /// The copies of the committed pages the writer freed, in ascending
    /// order of page id. Each is kept until the commit frees its page, so
    /// that no page step changes the page meanwhile.
    pub freed: Vec<PrivateRef<'s>>,
    /// The value pointers that entries stopped holding, in ascending order
    /// of file and offset.
    pub released: Vec<ValuePointer>,
    /// The ids of the pages the writer made and freed again. They go back
    /// to where they came from once no rollback of the transaction can
    /// bring such a page back (`transactions.md` ID1): at the commit, or
    /// with the transaction.
    pub unused: Vec<PageId>,
}

/// The flat pages of one writer of a write transaction: private copies in
/// the buffer pool, over the committed pages of the version the writer
/// reads.
///
/// - A page is copied when the tree takes it ([`FlatPagesMut::take`]), and
///   from then on background work leaves it alone (`transactions.md` PV7).
///   A committed page that the writer frees keeps its copy for the same
///   reason, until the commit.
/// - A value that goes out of line stays with the copy of its page, as the
///   bytes attached to the frame under the name its entry holds (WT4, FL7):
///   a pending value that no log names yet. [`Self::log_values`] gives each
///   its place in the commit's group, and that place is its name from then
///   on.
/// - A value pointer that an entry stops holding is kept for the commit to
///   release ([`Parts::released`]).
///
/// This is the flat keyspace's side of a write transaction, and not one:
/// it holds no latch between calls, takes no version and writes no file.
/// The caller makes its commit of [`Self::log_values`], [`Self::log_page`]
/// and [`Self::into_parts`].
///
/// The writer reads committed pages, so it borrows the guard of the
/// operation it works in, through [`Committed`]. Its copies are the pool's
/// and outlive that guard: [`Self::under`] takes the writer into the
/// transaction's next operation, and [`Parts`] is bound to no guard.
pub struct Private<'s, 'r, G: ?Sized, I> {
    base: Committed<'s, 'r, G>,
    pool: &'s BufferPool,
    owner: OwnerId,
    ids: I,
    copies: BTreeMap<PageId, Own<'s>>,
    freed: BTreeMap<PageId, PrivateRef<'s>>,
    unused: BTreeSet<PageId>,
    released: Vec<ValuePointer>,
    values: u64,
    revision: Revision,
}

impl<G: ?Sized, I> fmt::Debug for Private<'_, '_, G, I> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Private")
            .field("base", &self.base)
            .field("owner", &self.owner)
            .field("copies", &self.copies)
            .field("freed", &self.freed)
            .field("unused", &self.unused)
            .field("released", &self.released)
            .field("revision", &self.revision)
            .finish_non_exhaustive()
    }
}

/// The bit that the names of a writer's new values begin with: the address
/// of a value in the log has it only in a log of more than 2^63 bytes.
const NEW_VALUE: u64 = 1 << 63;

/// A revision that no pages of any writer of this process have had.
fn next_revision() -> Revision {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    Revision::Written(NEXT.fetch_add(1, Ordering::Relaxed))
}

impl<'s: 'r, 'r, G: OperationGuard + ?Sized, I: PageIds> Private<'s, 'r, G, I> {
    /// A writer over the committed pages `base`, whose copies are in `pool`
    /// under `owner` and whose new pages get their ids from `ids`.
    ///
    /// The writer is the only one that makes copies of flat pages for
    /// `owner`: a page that `owner` has a copy of already is refused when
    /// the writer takes it ([`CacheError::AlreadyPrivate`]).
    pub fn new(base: Committed<'s, 'r, G>, pool: &'s BufferPool, owner: OwnerId, ids: I) -> Self {
        Self {
            base,
            pool,
            owner,
            ids,
            copies: BTreeMap::new(),
            freed: BTreeMap::new(),
            unused: BTreeSet::new(),
            released: Vec::new(),
            values: 0,
            revision: next_revision(),
        }
    }

    /// The committed pages the writer reads.
    pub const fn base(&self) -> &Committed<'s, 'r, G> {
        &self.base
    }

    /// The writer, with all it holds, for the operation that holds `guard`:
    /// it reads the committed pages it read, at the same version, under
    /// that guard from now on.
    ///
    /// The version is one the transaction has pinned through both
    /// operations, as for [`Committed::under`]. A transaction that reads
    /// at another version from now on has rebased: it gives the writer up
    /// and makes its pages again (WT7).
    pub fn under<'q, H: OperationGuard + ?Sized>(self, guard: &'q H) -> Private<'s, 'q, H, I>
    where
        's: 'q,
    {
        Private {
            base: self.base.under(guard),
            pool: self.pool,
            owner: self.owner,
            ids: self.ids,
            copies: self.copies,
            freed: self.freed,
            unused: self.unused,
            released: self.released,
            values: self.values,
            revision: self.revision,
        }
    }

    /// The pages the writer has a copy of, in ascending order: those it
    /// changed, and those it made.
    pub fn written(&self) -> impl Iterator<Item = PageId> + '_ {
        self.copies.keys().copied()
    }

    /// The committed pages the writer freed, in ascending order.
    pub fn freed(&self) -> impl Iterator<Item = PageId> + '_ {
        self.freed.keys().copied()
    }

    /// The value pointers that entries stopped holding, in ascending order
    /// of file and offset: what the commit releases.
    pub fn released(&mut self) -> &[ValuePointer] {
        self.released
            .sort_unstable_by_key(|pointer| (pointer.file(), pointer.offset()));
        &self.released
    }

    /// Writes every value that an entry of the writer holds and no earlier
    /// commit has logged into `group`, each for the page of its entry, and
    /// gives each entry the pending value that names the value's place in
    /// that group (`log-format.md` LG20 to LG22). The pages come in
    /// ascending order, and the values of a page in key order.
    ///
    /// A commit calls this before [`Self::log_page`]. A group is written
    /// for one place (LG22): a commit that writes its group again for
    /// another place calls this again, and then [`Self::log_page`] again.
    /// Every call writes all of the writer's values and names them anew,
    /// and their bytes are with their frames under the new names when it
    /// returns. After a call that failed they need not be: the writer is
    /// given up ([`Self::roll_back`]), as after any call that failed.
    pub fn log_values(&mut self, group: &mut GroupWriter<'_>) -> Result<(), FlatError> {
        for (&page, own) in &mut self.copies {
            let mut latch = own.frame.write()?;
            // Every value of the writer leaves the copy's values before
            // one comes back under its place in the group, so that no name
            // stands for two values at any time.
            let mut logged = Vec::new();
            for (slot, named) in pending(&latch)? {
                let id = named.id();
                if own.values.get(&id).is_some_and(|held| held.inherited) {
                    continue;
                }
                let held = own.values.remove(&id);
                let held = held.ok_or(FlatError::NoValue { page })?;
                logged.push((slot, group.value(page, &held.bytes)?, held.bytes));
            }
            for (slot, name, bytes) in logged {
                if !stamp_pending(&mut latch, slot, name)? {
                    return Err(FlatError::NoValue { page });
                }
                let inherited = false;
                own.values.insert(name.id(), Held { bytes, inherited });
            }
            // The bytes follow their values to the new names.
            attach(&mut latch, page, &own.values)?;
        }
        self.revision = next_revision();
        Ok(())
    }

    /// Writes the changes the writer made to `page` into `group`, as a flat
    /// run, and says whether there are any: a page that is as it was has
    /// none, and no run.
    ///
    /// The pages of a group come in ascending order, tree pages and freed
    /// pages among them.
    pub fn log_page(&self, page: PageId, group: &mut GroupWriter<'_>) -> Result<bool, FlatError> {
        let Some(own) = self.copies.get(&page) else {
            return Ok(false);
        };
        let base = match own.created {
            true => None,
            false => Some(self.base.page(page)?),
        };
        let (base, image) = (base.as_deref(), own.frame.read());
        if !is_changed(base, &image)? {
            return Ok(false);
        }
        group.flat_run_with(page, |run| push_changes(base, &image, run).map(|_| ()))?;
        Ok(true)
    }

    /// Ends the writer and hands its copies to the commit, which installs
    /// the written ones, frees the pages of the others, releases the
    /// pointers and gives back the ids that were not used
    /// (`transactions.md` CM3 step 7).
    pub fn into_parts(mut self) -> Parts<'s> {
        self.released();
        Parts {
            written: self
                .copies
                .into_values()
                .map(|own| (own.frame, own.created))
                .collect(),
            freed: self.freed.into_values().collect(),
            released: self.released,
            unused: self.unused.into_iter().collect(),
        }
    }

    /// Gives the writer up: its copies are discarded, and the ids of the
    /// pages it made go back. This is what is left to do with a writer
    /// after a call that failed.
    ///
    /// Every copy is discarded and every id given back, also if one of
    /// them fails: the first error is returned. What could not be given
    /// back stays with the writer, an id with its copy, and a later call
    /// gives it back: nothing is given back twice. A writer that was given
    /// up whole holds nothing, and is as a new one.
    pub fn roll_back(&mut self) -> Result<(), FlatError> {
        let mut result = Ok(());
        let mut keep = |error: FlatError| {
            if result.is_ok() {
                result = Err(error);
            }
        };
        let pool = self.pool;
        self.copies
            .retain(|&page, own| match pool.discard(&own.frame) {
                Ok(()) => {
                    if own.created {
                        self.unused.insert(page);
                    }
                    false
                }
                Err(error) => {
                    keep(error.into());
                    true
                }
            });
        self.freed.retain(|_, copy| match pool.discard(copy) {
            Ok(()) => false,
            Err(error) => {
                keep(error.into());
                true
            }
        });
        let ids = &mut self.ids;
        self.unused.retain(|&page| match ids.release(page) {
            Ok(()) => false,
            Err(error) => {
                keep(error.into());
                true
            }
        });
        self.released.clear();
        self.revision = next_revision();
        result
    }

    /// The pending values of the copy `frame` of a committed page, which
    /// become the copy's: their bytes are attached to the frame, each under
    /// the name its entry holds.
    fn adopt(frame: &PrivateRef<'s>, page: PageId) -> Result<HashMap<ValueId, Held>, FlatError> {
        let mut values = HashMap::new();
        let mut latch = frame.write()?;
        for (_, pending) in pending(&latch)? {
            let name = pending.id();
            if values.contains_key(&name) {
                return Err(FlatError::Damaged {
                    page,
                    what: "two entries that hold one pending value",
                });
            }
            // The bytes that are with the page are the value only if they
            // are as long as the entry says.
            let bytes = latch
                .detach(name)
                .inspect(|bytes| drop(latch.attach(name, Arc::clone(bytes))))
                .filter(|bytes| bytes.len() == pending.value_len() as usize)
                .ok_or(FlatError::NoValue { page })?;
            let inherited = true;
            values.insert(name, Held { bytes, inherited });
        }
        Ok(values)
    }

    /// Whether the writer freed `page`.
    fn is_freed(&self, page: PageId) -> bool {
        self.freed.contains_key(&page) || self.unused.contains(&page)
    }

    /// The writer's copy of `page`, made now if it has none.
    fn own(&mut self, page: PageId) -> Result<&mut Own<'s>, FlatError> {
        if self.is_freed(page) {
            return Err(FlatError::Freed { page });
        }
        if !self.copies.contains_key(&page) {
            let (owner, at) = (self.owner, self.base.at);
            // What was loaded is held while the copy is made. A page step
            // can give the page another frame meanwhile, which the pool
            // need not hold: then that one is loaded, until the copy is
            // made or the page is not there to load.
            //
            // A copy that the pool has for this owner and the writer does
            // not know was left by another writer, and is not this
            // writer's to change. The pool refuses it in the step that
            // would make the copy, so the copy the writer is given is one
            // that nobody else was given.
            let mut loaded = None;
            let frame = loop {
                match self.pool.make_private_once(owner, page, at) {
                    Ok(frame) => break frame,
                    Err(NotMade::AlreadyPrivate) => {
                        return Err(CacheError::AlreadyPrivate { page, owner }.into())
                    }
                    Err(NotMade::Miss(miss)) => {
                        let buffers = &mut self.base.buffers.borrow_mut().0;
                        loaded = Some(self.base.loader.load(miss, buffers)?);
                    }
                }
            };
            drop(loaded);
            let values = match Self::adopt(&frame, page) {
                Ok(values) => values,
                Err(error) => {
                    // The copy is of no use, and must not stand in the way
                    // of page steps. One that the pool will not discard now
                    // stays the writer's, to be discarded with the writer.
                    if self.pool.discard(&frame).is_err() {
                        let (created, values) = (false, HashMap::new());
                        let own = Own {
                            frame,
                            created,
                            values,
                        };
                        self.copies.insert(page, own);
                    }
                    return Err(error);
                }
            };
            let created = false;
            let own = Own {
                frame,
                created,
                values,
            };
            self.copies.insert(page, own);
        }
        self.copies.get_mut(&page).ok_or(FlatError::Freed { page })
    }

    /// Holds `bytes` as a value of `page` that no log has, under a name
    /// that no other value of the page has.
    fn hold(&mut self, page: PageId, bytes: Arc<[u8]>) -> Result<ValueId, FlatError> {
        let mut next = self.values;
        let values = &mut self.own(page)?.values;
        let name = loop {
            next += 1;
            let name = (NEW_VALUE | next).to_le_bytes();
            if !values.contains_key(&name) {
                break name;
            }
        };
        let inherited = false;
        values.insert(name, Held { bytes, inherited });
        self.values = next;
        Ok(name)
    }
}

/// Attaches to the frame under `latch` the bytes of the pending values its
/// leaf holds, each under its name, and nothing else.
fn attach(
    latch: &mut ExclusiveLatch<'_>,
    page: PageId,
    values: &HashMap<ValueId, Held>,
) -> Result<(), FlatError> {
    let old: Vec<ValueId> = latch.attachments().map(|(name, _)| name).collect();
    for name in old {
        latch.detach(name);
    }
    for (_, pending) in pending(latch)? {
        let held = held(values, pending, page)?;
        latch.attach(pending.id(), Arc::clone(&held.bytes));
    }
    Ok(())
}

/// The value of `values` that `pending`, which an entry of `page` holds,
/// names: one that is as long as the entry says.
fn held(
    values: &HashMap<ValueId, Held>,
    pending: PendingValue,
    page: PageId,
) -> Result<&Held, FlatError> {
    values
        .get(&pending.id())
        .filter(|held| held.bytes.len() == pending.value_len() as usize)
        .ok_or(FlatError::NoValue { page })
}

/// Checks that each pending value the leaf `image` of `page` holds is one
/// of `values`, as long as its entry says, and held by that entry alone.
fn check_values(
    image: &[u8],
    page: PageId,
    values: &HashMap<ValueId, Held>,
) -> Result<(), FlatError> {
    let mut names = HashSet::new();
    for (_, pending) in pending(image)? {
        held(values, pending, page)?;
        if !names.insert(pending.id()) {
            return Err(FlatError::Damaged {
                page,
                what: "two entries that hold one pending value",
            });
        }
    }
    Ok(())
}

impl<'s: 'r, 'r, G: OperationGuard + ?Sized, I: PageIds> FlatPages for Private<'s, 'r, G, I> {
    type Page<'a>
        = PrivatePage<'a, 'r>
    where
        Self: 'a;

    fn page(&self, page: PageId) -> Result<PrivatePage<'_, 'r>, FlatError> {
        if self.is_freed(page) {
            return Err(FlatError::Freed { page });
        }
        match self.copies.get(&page) {
            Some(own) => Ok(PrivatePage::Own(own.frame.read())),
            None => Ok(PrivatePage::Committed(self.base.page(page)?)),
        }
    }

    fn read_value(
        &self,
        held: &PrivatePage<'_, 'r>,
        page: PageId,
        _slot: usize,
        place: ValuePlace,
        out: &mut Vec<u8>,
    ) -> Result<(), FlatError> {
        let pending = match place {
            ValuePlace::Placed(pointer) => return self.base.read_placed(pointer, page, out),
            ValuePlace::Pending(pending) => pending,
        };
        // The writer keeps the pending values of its copies by their
        // names. Those of a committed page are with its frame.
        match (self.copies.get(&page), held) {
            (Some(own), _) => {
                let held = own.values.get(&pending.id());
                let held = held.ok_or(FlatError::NoValue { page })?;
                out.extend_from_slice(&held.bytes);
                Ok(())
            }
            (None, PrivatePage::Committed(frame)) => read_attached(frame, pending, out),
            (None, PrivatePage::Own(_)) => Err(FlatError::NoValue { page }),
        }
    }

    fn revision(&self) -> Revision {
        self.revision
    }
}

impl<'s: 'r, 'r, G: OperationGuard + ?Sized, I: PageIds> FlatPagesMut for Private<'s, 'r, G, I> {
    fn allocate(&mut self) -> Result<PageId, FlatError> {
        let page = self.ids.allocate()?;
        let frame = match self.pool.new_private(self.owner, page) {
            Ok(frame) => frame,
            Err(error) => {
                // The id goes back when the writer ends.
                self.unused.insert(page);
                return Err(error.into());
            }
        };
        let (created, values) = (true, HashMap::new());
        self.copies.insert(
            page,
            Own {
                frame,
                created,
                values,
            },
        );
        Ok(page)
    }

    fn take(&mut self, page: PageId) -> Result<Vec<u8>, FlatError> {
        let latch = self.own(page)?.frame.read();
        Ok(view(&latch)?.as_bytes().to_vec())
    }

    fn write(&mut self, page: PageId, image: &[u8]) -> Result<(), FlatError> {
        let own = self.own(page)?;
        let mut latch = own.frame.write()?;
        if image.len() > latch.len() {
            return Err(FlatError::Damaged {
                page,
                what: "an image longer than a frame",
            });
        }
        // An image whose values the writer does not hold as its entries
        // say is refused before the copy changes: what the copy holds and
        // what is with its frame stay as they were.
        check_values(image, page, &own.values)?;
        let (head, tail) = latch.split_at_mut(image.len());
        head.copy_from_slice(image);
        tail.fill(0);
        attach(&mut latch, page, &own.values)?;
        drop(latch);
        self.revision = next_revision();
        Ok(())
    }

    fn free(&mut self, page: PageId) -> Result<(), FlatError> {
        if self.own(page)?.created {
            // Nobody else has seen the page. Its id is kept until the
            // transaction knows that no rollback brings the page back. A
            // copy that cannot be discarded stays the writer's.
            let own = self.copies.get(&page).ok_or(FlatError::Freed { page })?;
            self.pool.discard(&own.frame)?;
            self.copies.remove(&page);
            self.unused.insert(page);
        } else if let Some(own) = self.copies.remove(&page) {
            self.freed.insert(page, own.frame);
        }
        self.revision = next_revision();
        Ok(())
    }

    fn hold_value(&mut self, page: PageId, value: &[u8]) -> Result<PendingValue, FlatError> {
        let len = value_len(value)?;
        let name = self.hold(page, Arc::from(value))?;
        Ok(PendingValue::new(name, len)?)
    }

    fn drop_value(&mut self, page: PageId, place: ValuePlace) -> Result<(), FlatError> {
        match place {
            ValuePlace::Placed(pointer) => self.released.push(pointer),
            ValuePlace::Pending(pending) => {
                self.own(page)?.values.remove(&pending.id());
            }
        }
        Ok(())
    }

    fn carry_value(
        &mut self,
        from: PageId,
        _slot: usize,
        to: PageId,
        place: ValuePlace,
    ) -> Result<PendingValue, FlatError> {
        // The bytes go to the new page as they are. A value that was in
        // the value log is read once, and its pointer is released.
        let bytes = match place {
            ValuePlace::Pending(pending) => {
                let held = self.own(from)?.values.remove(&pending.id());
                held.ok_or(FlatError::NoValue { page: from })?.bytes
            }
            ValuePlace::Placed(pointer) => {
                let mut value = Vec::new();
                self.base.read_placed(pointer, from, &mut value)?;
                self.released.push(pointer);
                Arc::from(value)
            }
        };
        let name = self.hold(to, bytes)?;
        Ok(PendingValue::new(name, place.value_len())?)
    }
}
