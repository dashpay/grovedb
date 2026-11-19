//! The pages and the values of one version, as the recomputation of a hash
//! reads them (`lignin_tree::hash::check`).

use std::{cell::RefCell, error::Error, fmt, rc::Rc};

use lignin_format::{
    value_log::{ValueLogError, ValuePlace},
    CommitVersion, FormatError, PageId,
};
use lignin_store::{
    cache::PageRef,
    load::{LoadBuffers, LoadError, PageLoader},
    values::{ValueStore, ValueStoreError},
};
use lignin_tree::{
    fragment::{FragmentError, PageMut, Pages},
    hash::{Refusal, ValueReader},
    node::{Lease, NodeError, NodePage},
};

/// What a checker answers to a call that would change a page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ReadOnly;

impl fmt::Display for ReadOnly {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("the invariant checker changes no page")
    }
}

impl Error for ReadOnly {}

/// The pages of one version as one step of a run reads them: each page of
/// the step from one frame, however often it is asked for.
///
/// A page version can get another frame while a run reads it: write-back
/// puts a pointer where a pending value was, and takes the value's bytes
/// off the frame (`transactions.md` PV7). What is read of a cell and the
/// bytes of its pending value are of one frame only if they are read from
/// the same one (`lignin_tree::value`, "A value is read for the page that
/// is held"). So the frames that a step has read stay held until the next
/// step begins, and the walk, the recomputation of a hash and the reader of
/// the values all read through this.
pub(super) struct Frames<'s> {
    loader: PageLoader<'s>,
    at: CommitVersion,
    buffers: LoadBuffers,
    held: Vec<(PageId, PageRef<'s>)>,
}

impl<'s> Frames<'s> {
    /// The frames that are kept from one step to the next.
    const KEPT: usize = 8;

    pub(super) fn new(loader: PageLoader<'s>, at: CommitVersion) -> Shared<'s> {
        Rc::new(RefCell::new(Self {
            loader,
            at,
            buffers: LoadBuffers::new(),
            held: Vec::new(),
        }))
    }

    /// Begins a step: the frames of the steps before are let go once they
    /// are more than a few.
    pub(super) fn begin(&mut self) {
        if self.held.len() > Self::KEPT {
            self.held.clear();
        }
    }

    /// The page `page` as the version has it, pinned: the frame this step
    /// read it from before, if it did.
    pub(super) fn get(&mut self, page: PageId) -> Result<PageRef<'s>, LoadError> {
        if let Some((_, frame)) = self.held.iter().find(|(held, _)| *held == page) {
            return Ok(frame.clone());
        }
        let frame = self.loader.get(page, self.at, &mut self.buffers)?;
        self.held.push((page, frame.clone()));
        Ok(frame)
    }
}

/// The frames of a run, which its readers share.
pub(super) type Shared<'s> = Rc<RefCell<Frames<'s>>>;

/// The committed tree pages of one version. Nothing is written through it.
pub(super) struct TreeRead<'s> {
    frames: Shared<'s>,
    lease: Lease,
}

impl<'s> TreeRead<'s> {
    pub(super) fn new(frames: Shared<'s>, lease: Lease) -> Self {
        Self { frames, lease }
    }
}

impl Pages for TreeRead<'_> {
    fn read<R>(
        &mut self,
        page: PageId,
        read: impl FnOnce(NodePage<'_>) -> R,
    ) -> Result<R, FragmentError> {
        let frame = (self.frames.borrow_mut().get(page))
            .map_err(|error| FragmentError::Pages(Box::new(error)))?;
        Ok(read(NodePage::open(&frame, self.lease)?))
    }

    fn write<R>(
        &mut self,
        _: PageId,
        _: impl FnOnce(&mut PageMut<'_, '_>) -> Result<R, NodeError>,
    ) -> Result<R, FragmentError> {
        Err(FragmentError::Pages(Box::new(ReadOnly)))
    }

    fn create(&mut self) -> Result<PageId, FragmentError> {
        Err(FragmentError::Pages(Box::new(ReadOnly)))
    }

    fn free(&mut self, _: PageId) -> Result<(), FragmentError> {
        Err(FragmentError::Pages(Box::new(ReadOnly)))
    }
}

/// Why a value was not read.
#[derive(Debug)]
pub(super) enum ValueFault {
    /// The value's record names another page as its owner.
    Owner(ValueStoreError),
    /// The value is not read from the value log.
    Unreadable(ValueStoreError),
    /// The page of a pending value is not read.
    Page(LoadError),
    /// A pending value has no bytes with its page, or bytes of another
    /// length.
    NoBytes {
        /// The bytes with the page, if there are any.
        found: Option<usize>,
    },
}

impl fmt::Display for ValueFault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Owner(error) | Self::Unreadable(error) => error.fmt(f),
            Self::Page(error) => error.fmt(f),
            Self::NoBytes { found: None } => f.write_str("no bytes are with the page"),
            Self::NoBytes { found: Some(len) } => write!(f, "{len} bytes are with the page"),
        }
    }
}

impl Error for ValueFault {}

/// The values of one version that are not in their cells: read from the
/// value log with the page of their cell as the owner, or from the bytes
/// that are with the frame of that page.
///
/// It keeps the value it read last, so that a value which is checked and
/// then hashed is read once.
pub(super) struct ValueRead<'s> {
    store: &'s ValueStore,
    frames: Shared<'s>,
    last: Option<(ValuePlace, PageId)>,
    bytes: Vec<u8>,
    scratch: Vec<u8>,
}

impl<'s> ValueRead<'s> {
    pub(super) fn new(store: &'s ValueStore, frames: Shared<'s>) -> Self {
        Self {
            store,
            frames,
            last: None,
            bytes: Vec::new(),
            scratch: Vec::new(),
        }
    }

    /// Forgets the value that was read last: a step of the run begins, and
    /// its pages may be read from other frames.
    pub(super) fn begin(&mut self) {
        self.last = None;
    }

    /// The bytes of the value at `place`, which a cell or a flat record of
    /// the page `page` holds.
    pub(super) fn fetch(&mut self, place: ValuePlace, page: PageId) -> Result<&[u8], ValueFault> {
        if self.last == Some((place, page)) {
            return Ok(&self.bytes);
        }
        self.last = None;
        self.bytes.clear();
        match place {
            ValuePlace::Placed(pointer) => {
                let value = (self.store.read(pointer, page, &mut self.scratch)).map_err(
                    |error| match error {
                        ValueStoreError::Format {
                            source: FormatError::ValueLog(ValueLogError::WrongOwner { .. }),
                            ..
                        } => ValueFault::Owner(error),
                        error => ValueFault::Unreadable(error),
                    },
                )?;
                value
                    .parts()
                    .for_each(|part| self.bytes.extend_from_slice(part));
            }
            ValuePlace::Pending(pending) => {
                let frame = (self.frames.borrow_mut().get(page)).map_err(ValueFault::Page)?;
                let found = frame.attached(pending.id());
                let bytes = found
                    .filter(|bytes| bytes.len() == pending.value_len() as usize)
                    .ok_or(ValueFault::NoBytes {
                        found: found.map(<[u8]>::len),
                    })?;
                self.bytes.extend_from_slice(bytes);
            }
        }
        self.last = Some((place, page));
        Ok(&self.bytes)
    }
}

impl ValueReader for ValueRead<'_> {
    fn read(&mut self, place: ValuePlace, page: PageId, into: &mut Vec<u8>) -> Result<(), Refusal> {
        let bytes = self.fetch(place, page)?;
        into.extend_from_slice(bytes);
        Ok(())
    }
}
