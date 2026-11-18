//! The pages of a flat keyspace in the page store: committed pages through
//! the loader, and the private copies of one writer in the buffer pool.

use std::{
    cell::RefCell,
    collections::{BTreeMap, HashMap},
    ops::Deref,
    sync::Arc,
};

use lignin_format::{
    flat_page::FlatValue,
    value_log::{PendingValue, ValuePlace, ValuePointer},
    wal::GroupWriter,
    CommitVersion, PageId,
};
use lignin_store::{
    alloc::{AllocError, PageAllocator},
    cache::{BufferPool, ExclusiveLatch, OwnerId, PageRef, PrivateRef, SharedLatch},
    load::{LoadBuffers, PageLoader},
    values::ValueStore,
};

use super::{
    error::FlatError,
    log::{is_changed, push_changes, stamp_pending},
    node::{place, view},
    pages::{value_len, FlatPages, FlatPagesMut},
};

/// The tag under which the bytes of a pending value are attached to the
/// frame of its page: the position of the entry that holds it.
fn tag(slot: usize) -> Option<u32> {
    u32::try_from(slot).ok()
}

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
/// frame of its page, where its bytes are attached under the position of
/// the entry that holds it (`transactions.md` FL7).
///
/// The reader holds an operation guard for as long as it uses this, and
/// reads at a version it has pinned, or at the latest version it read under
/// the guard (PN5).
#[derive(Debug)]
pub struct Committed<'s> {
    loader: PageLoader<'s>,
    values: &'s ValueStore,
    at: CommitVersion,
    buffers: RefCell<(LoadBuffers, Vec<u8>)>,
}

impl<'s> Committed<'s> {
    /// The pages of `loader`'s store at version `at`, with their values out
    /// of line in `values`.
    pub fn new(loader: PageLoader<'s>, values: &'s ValueStore, at: CommitVersion) -> Self {
        Self {
            loader,
            values,
            at,
            buffers: RefCell::default(),
        }
    }

    /// The version the pages are read at.
    pub const fn at(&self) -> CommitVersion {
        self.at
    }
}

impl<'s> FlatPages for Committed<'s> {
    type Page<'a>
        = PageRef<'s>
    where
        Self: 'a;

    fn page(&self, page: PageId) -> Result<PageRef<'s>, FlatError> {
        let buffers = &mut self.buffers.borrow_mut().0;
        Ok(self.loader.get(page, self.at, buffers)?)
    }

    fn read_value(
        &self,
        page: PageId,
        slot: usize,
        place: ValuePlace,
        out: &mut Vec<u8>,
    ) -> Result<(), FlatError> {
        match place {
            ValuePlace::Placed(pointer) => {
                let buf = &mut self.buffers.borrow_mut().1;
                let value = self.values.read(pointer, page, buf)?;
                value.parts().for_each(|part| out.extend_from_slice(part));
            }
            ValuePlace::Pending(pending) => {
                let frame = self.page(page)?;
                let bytes = tag(slot)
                    .and_then(|tag| frame.attached(tag))
                    .filter(|bytes| bytes.len() == pending.value_len() as usize)
                    .ok_or(FlatError::NoValue { page })?;
                out.extend_from_slice(bytes);
            }
        }
        Ok(())
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
    /// Whether a log group holds the value: it was pending in the page
    /// when the copy was made, or the commit has written it.
    logged: bool,
}

/// One private copy of a writer.
#[derive(Debug)]
struct Own<'s> {
    frame: PrivateRef<'s>,
    /// Whether the writer made the page.
    created: bool,
    /// The pending values of the page, by the 8 bytes that name them.
    values: HashMap<[u8; PendingValue::ID_LEN], Held>,
}

/// A page as a writer reads it: its own copy, or the committed page.
#[derive(Debug)]
pub enum PrivatePage<'a, 's> {
    /// A committed page that the writer has not copied.
    Committed(PageRef<'s>),
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
    /// The copies of the pages the writer changed or made, in ascending
    /// order of page id, each with whether the writer made the page.
    pub written: Vec<(PrivateRef<'s>, bool)>,
    /// The copies of the committed pages the writer freed, in ascending
    /// order of page id. Each is kept until the commit frees its page, so
    /// that no page step changes the page meanwhile.
    pub freed: Vec<PrivateRef<'s>>,
    /// The value pointers that entries stopped holding, in ascending order
    /// of file and offset.
    pub released: Vec<ValuePointer>,
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
///   bytes attached to the frame (WT4, FL7), and its entry holds a pending
///   value that no log names yet. [`Self::log_values`] gives each its place
///   in the commit's group.
/// - A value pointer that an entry stops holding is kept for the commit to
///   release ([`Parts::released`]).
///
/// This is the flat keyspace's side of a write transaction, and not one:
/// it holds no latch between calls, takes no version and writes no file.
/// The caller makes its commit of [`Self::log_values`], [`Self::log_page`]
/// and [`Self::into_parts`].
#[derive(Debug)]
pub struct Private<'s, I> {
    base: Committed<'s>,
    pool: &'s BufferPool,
    owner: OwnerId,
    ids: I,
    copies: BTreeMap<PageId, Own<'s>>,
    freed: BTreeMap<PageId, PrivateRef<'s>>,
    released: Vec<ValuePointer>,
    values: u64,
    revision: u64,
}

/// The bit that tells the name of a value no log holds yet from a log
/// address, which is below it.
const UNLOGGED: u64 = 1 << 63;

impl<'s, I: PageIds> Private<'s, I> {
    /// A writer over the committed pages `base`, whose copies are in `pool`
    /// under `owner` and whose new pages get their ids from `ids`.
    pub fn new(base: Committed<'s>, pool: &'s BufferPool, owner: OwnerId, ids: I) -> Self {
        Self {
            base,
            pool,
            owner,
            ids,
            copies: BTreeMap::new(),
            freed: BTreeMap::new(),
            released: Vec::new(),
            values: 0,
            revision: 0,
        }
    }

    /// The committed pages the writer reads.
    pub const fn base(&self) -> &Committed<'s> {
        &self.base
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

    /// Writes every value that the writer's entries hold and no log has
    /// into `group`, each for the page of its entry, and gives each entry
    /// the pending value that names the value's place in the log
    /// (`log-format.md` LG20 to LG22). The pages come in ascending order,
    /// and the values of a page in key order.
    ///
    /// A commit calls this before [`Self::log_page`].
    pub fn log_values(&mut self, group: &mut GroupWriter<'_>) -> Result<(), FlatError> {
        for (&page, own) in &mut self.copies {
            let mut latch = own.frame.write()?;
            for (_, unlogged) in pending(&latch)? {
                let Some(held) = own.values.remove(&unlogged.id()) else {
                    return Err(FlatError::NoValue { page });
                };
                let name = if held.logged {
                    unlogged
                } else {
                    let logged = group.value(page, &held.bytes)?;
                    stamp_pending(&mut latch, unlogged, logged)?;
                    logged
                };
                let (bytes, logged) = (held.bytes, true);
                own.values.insert(name.id(), Held { bytes, logged });
            }
        }
        self.revision += 1;
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
    /// the written ones, frees the pages of the others, and releases the
    /// pointers (`transactions.md` CM3 step 7). A rollback discards the
    /// copies and gives back the ids of the pages the writer made.
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
        }
    }

    /// The writer's copy of `page`, made now if it has none.
    fn own(&mut self, page: PageId) -> Result<&mut Own<'s>, FlatError> {
        if self.freed.contains_key(&page) {
            return Err(FlatError::Freed { page });
        }
        if !self.copies.contains_key(&page) {
            let at = self.base.at;
            let frame = match self.pool.make_private(self.owner, page, at) {
                Ok(frame) => frame,
                Err(miss) => {
                    // What was loaded is held until the copy is made.
                    let buffers = &mut self.base.buffers.borrow_mut().0;
                    let _loaded = self.base.loader.load(miss, buffers)?;
                    let copy = self.pool.make_private(self.owner, page, at);
                    copy.map_err(|_| FlatError::Damaged {
                        page,
                        what: "a page that left the pool while it was held",
                    })?
                }
            };
            // The values that are pending in the page are the copy's now.
            let mut values = HashMap::new();
            let mut latch = frame.write()?;
            for (slot, pending) in pending(&latch)? {
                let bytes = tag(slot)
                    .and_then(|tag| Some((tag, latch.detach(tag)?)))
                    .map(|(tag, bytes)| {
                        latch.attach(tag, Arc::clone(&bytes));
                        bytes
                    })
                    .ok_or(FlatError::NoValue { page })?;
                let logged = true;
                values.insert(pending.id(), Held { bytes, logged });
            }
            drop(latch);
            let created = false;
            self.copies.insert(
                page,
                Own {
                    frame,
                    created,
                    values,
                },
            );
        }
        self.copies.get_mut(&page).ok_or(FlatError::Freed { page })
    }

    fn name(&mut self) -> [u8; PendingValue::ID_LEN] {
        self.values += 1;
        (UNLOGGED | self.values).to_le_bytes()
    }
}

/// Attaches to the frame under `latch` the bytes of the pending values its
/// leaf holds, each under the position of its entry, and nothing else.
fn attach(
    latch: &mut ExclusiveLatch<'_>,
    page: PageId,
    values: &HashMap<[u8; PendingValue::ID_LEN], Held>,
) -> Result<(), FlatError> {
    let old: Vec<u32> = latch.attachments().map(|(tag, _)| tag).collect();
    for tag in old {
        latch.detach(tag);
    }
    for (slot, pending) in pending(latch)? {
        let (Some(tag), Some(held)) = (tag(slot), values.get(&pending.id())) else {
            return Err(FlatError::NoValue { page });
        };
        latch.attach(tag, Arc::clone(&held.bytes));
    }
    Ok(())
}

impl<'s, I: PageIds> FlatPages for Private<'s, I> {
    type Page<'a>
        = PrivatePage<'a, 's>
    where
        Self: 'a;

    fn page(&self, page: PageId) -> Result<PrivatePage<'_, 's>, FlatError> {
        if self.freed.contains_key(&page) {
            return Err(FlatError::Freed { page });
        }
        match self.copies.get(&page) {
            Some(own) => Ok(PrivatePage::Own(own.frame.read())),
            None => Ok(PrivatePage::Committed(self.base.page(page)?)),
        }
    }

    fn read_value(
        &self,
        page: PageId,
        slot: usize,
        place: ValuePlace,
        out: &mut Vec<u8>,
    ) -> Result<(), FlatError> {
        match (place, self.copies.get(&page)) {
            (ValuePlace::Pending(pending), Some(own)) => {
                let held = own.values.get(&pending.id());
                let held = held.ok_or(FlatError::NoValue { page })?;
                out.extend_from_slice(&held.bytes);
                Ok(())
            }
            _ => self.base.read_value(page, slot, place, out),
        }
    }

    fn revision(&self) -> u64 {
        self.revision
    }
}

impl<I: PageIds> FlatPagesMut for Private<'_, I> {
    fn allocate(&mut self) -> Result<PageId, FlatError> {
        let page = self.ids.allocate()?;
        let frame = self.pool.new_private(self.owner, page)?;
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
        let Some((head, tail)) = latch.split_at_mut_checked(image.len()) else {
            return Err(FlatError::Damaged {
                page,
                what: "an image longer than a frame",
            });
        };
        head.copy_from_slice(image);
        tail.fill(0);
        attach(&mut latch, page, &own.values)?;
        drop(latch);
        self.revision += 1;
        Ok(())
    }

    fn free(&mut self, page: PageId) -> Result<(), FlatError> {
        self.own(page)?;
        let Some(own) = self.copies.remove(&page) else {
            return Err(FlatError::Freed { page });
        };
        if own.created {
            self.pool.discard(&own.frame)?;
            self.ids.release(page)?;
        } else {
            self.freed.insert(page, own.frame);
        }
        self.revision += 1;
        Ok(())
    }

    fn hold_value(&mut self, page: PageId, value: &[u8]) -> Result<PendingValue, FlatError> {
        let len = value_len(value)?;
        let name = self.name();
        let (bytes, logged) = (Arc::from(value), false);
        self.own(page)?.values.insert(name, Held { bytes, logged });
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
        slot: usize,
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
                self.read_value(from, slot, place, &mut value)?;
                self.released.push(pointer);
                Arc::from(value)
            }
        };
        let name = self.name();
        let logged = false;
        self.own(to)?.values.insert(name, Held { bytes, logged });
        Ok(PendingValue::new(name, place.value_len())?)
    }
}
