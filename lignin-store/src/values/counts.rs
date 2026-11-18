//! The count of live values of each value-log file, and the changes the
//! layers above report to it.

use lignin_format::{value_log::ValuePointer, CommitVersion, FileId};

/// What one change point does to the value log: the pointers that cells
/// start to hold and the pointers that cells stop holding.
///
/// A change point is the commit of a transaction or one page step of
/// background work. The layers above build one of these for each, store it
/// with the pages it belongs to, and hand it to
/// [`ValueStore::apply`](super::ValueStore::apply) at that point.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ValueChange {
    acquired: Vec<ValuePointer>,
    released: Vec<ValuePointer>,
}

impl ValueChange {
    /// A change that touches no pointer.
    pub fn new() -> Self {
        Self::default()
    }

    /// The change a move makes: the cell that held `from` holds `to`.
    pub fn moved(from: ValuePointer, to: ValuePointer) -> Self {
        Self {
            acquired: vec![to],
            released: vec![from],
        }
    }

    /// A cell holds `pointer` from this change point on.
    pub fn acquire(&mut self, pointer: ValuePointer) {
        self.acquired.push(pointer);
    }

    /// A cell that held `pointer` before this change point no longer does:
    /// the cell was deleted, was given another value, had its value appended
    /// again, or went with a dropped page.
    pub fn release(&mut self, pointer: ValuePointer) {
        self.released.push(pointer);
    }

    /// The pointers cells start to hold.
    pub fn acquired(&self) -> &[ValuePointer] {
        &self.acquired
    }

    /// The pointers cells stop holding.
    pub fn released(&self) -> &[ValuePointer] {
        &self.released
    }

    /// Whether the change touches no pointer.
    pub fn is_empty(&self) -> bool {
        self.acquired.is_empty() && self.released.is_empty()
    }
}

/// The live values of one value-log file, as a checkpoint of the pages
/// stores them and [`ValueStore::recover`](super::ValueStore::recover) takes
/// them back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileCount {
    /// The file.
    pub file: FileId,
    /// How many cells of the latest version hold a pointer into the file.
    pub values: u64,
    /// The bytes those values take in the file, record headers included.
    pub bytes: u64,
    /// The count was found to be wrong: a pointer was released that it did
    /// not include. The file is kept whatever the count says.
    pub miscounted: bool,
}

/// A place in the sequence of changes applied to the counts.
///
/// [`ValueStore::applied`](super::ValueStore::applied) gives the place of
/// the last change applied, and
/// [`ValueStore::mark_durable`](super::ValueStore::mark_durable) takes it
/// back once every change up to it is durable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct ChangeMark(pub(super) u64);

/// When a file's count last became zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Emptied {
    /// The latest commit version at that change point.
    pub(super) version: CommitVersion,
    /// The place of that change.
    pub(super) change: u64,
}

impl Emptied {
    pub(super) const NEVER_HELD: Self = Self {
        version: CommitVersion::EMPTY,
        change: 0,
    };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Account {
    pub(super) values: u64,
    pub(super) bytes: u64,
    pub(super) miscounted: bool,
    pub(super) emptied: Emptied,
    /// The place of the last change that touched the account.
    pub(super) changed: u64,
    /// The place of the last change that added a value to the account.
    pub(super) acquired: u64,
    /// The place the changes had reached when a cleaning pass last left the
    /// file with live values.
    pub(super) cleaned: Option<u64>,
}

impl Account {
    pub(super) const EMPTY: Self = Self {
        values: 0,
        bytes: 0,
        miscounted: false,
        emptied: Emptied::NEVER_HELD,
        changed: 0,
        acquired: 0,
        cleaned: None,
    };

    pub(super) fn from_count(count: &FileCount) -> Self {
        Self {
            values: count.values,
            bytes: count.bytes,
            miscounted: count.miscounted,
            ..Self::EMPTY
        }
    }

    /// The account as a session takes it over from recovery: with the
    /// counts recovery reached, and empty, if it is, since `change`.
    pub(super) fn handed_over(&self, change: u64) -> Self {
        Self {
            values: self.values,
            bytes: self.bytes,
            miscounted: self.miscounted,
            emptied: Emptied {
                version: CommitVersion::EMPTY,
                change,
            },
            ..Self::EMPTY
        }
    }

    pub(super) fn count(&self, file: FileId) -> FileCount {
        FileCount {
            file,
            values: self.values,
            bytes: self.bytes,
            miscounted: self.miscounted,
        }
    }

    pub(super) fn is_counted(&self) -> bool {
        self.values != 0 || self.miscounted
    }

    pub(super) fn acquire(&mut self, extent: u64, change: u64) {
        self.values = self.values.saturating_add(1);
        self.bytes = self.bytes.saturating_add(extent);
        self.changed = change;
        self.acquired = change;
    }

    /// Takes one value off the count. Returns `false`, and marks the count
    /// wrong, if the count was already zero.
    pub(super) fn release(&mut self, extent: u64, latest: CommitVersion, change: u64) -> bool {
        self.changed = change;
        if self.values == 0 {
            self.miscounted = true;
            return false;
        }
        self.values -= 1;
        self.bytes = self.bytes.saturating_sub(extent);
        if self.values == 0 {
            self.bytes = 0;
            self.emptied = Emptied {
                version: latest,
                change,
            };
        }
        true
    }
}
