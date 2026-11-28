//! Injected failures: which operations fail, with which error, and how the
//! error says so.

use std::{
    error::Error,
    fmt, io,
    ops::BitOr,
    path::{Path, PathBuf},
};

use super::rng::Chance;

/// The operations of [`Vfs`](crate::Vfs) and [`VfsFile`](crate::VfsFile).
///
/// [`VfsFile::read_exact_at`](crate::VfsFile::read_exact_at) is a
/// [`Read`](Self::Read).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum OpKind {
    /// [`Vfs::open`](crate::Vfs::open)
    Open,
    /// [`Vfs::rename`](crate::Vfs::rename)
    Rename,
    /// [`Vfs::hard_link`](crate::Vfs::hard_link)
    HardLink,
    /// [`Vfs::remove_file`](crate::Vfs::remove_file)
    RemoveFile,
    /// [`Vfs::create_dir`](crate::Vfs::create_dir)
    CreateDir,
    /// [`Vfs::remove_dir`](crate::Vfs::remove_dir)
    RemoveDir,
    /// [`Vfs::read_dir`](crate::Vfs::read_dir)
    ReadDir,
    /// [`Vfs::sync_dir`](crate::Vfs::sync_dir)
    SyncDir,
    /// [`Vfs::lock`](crate::Vfs::lock)
    Lock,
    /// [`VfsFile::read_at`](crate::VfsFile::read_at)
    Read,
    /// [`VfsFile::write_at`](crate::VfsFile::write_at)
    Write,
    /// [`VfsFile::append`](crate::VfsFile::append)
    Append,
    /// [`VfsFile::sync_data`](crate::VfsFile::sync_data)
    SyncData,
    /// [`VfsFile::sync_all`](crate::VfsFile::sync_all)
    SyncAll,
    /// [`VfsFile::len`](crate::VfsFile::len)
    Len,
    /// [`VfsFile::set_len`](crate::VfsFile::set_len)
    SetLen,
}

impl OpKind {
    /// Every kind, in the order of their codes.
    pub const ALL: [Self; 16] = [
        Self::Open,
        Self::Rename,
        Self::HardLink,
        Self::RemoveFile,
        Self::CreateDir,
        Self::RemoveDir,
        Self::ReadDir,
        Self::SyncDir,
        Self::Lock,
        Self::Read,
        Self::Write,
        Self::Append,
        Self::SyncData,
        Self::SyncAll,
        Self::Len,
        Self::SetLen,
    ];

    /// The name of the method.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Rename => "rename",
            Self::HardLink => "hard_link",
            Self::RemoveFile => "remove_file",
            Self::CreateDir => "create_dir",
            Self::RemoveDir => "remove_dir",
            Self::ReadDir => "read_dir",
            Self::SyncDir => "sync_dir",
            Self::Lock => "lock",
            Self::Read => "read_at",
            Self::Write => "write_at",
            Self::Append => "append",
            Self::SyncData => "sync_data",
            Self::SyncAll => "sync_all",
            Self::Len => "len",
            Self::SetLen => "set_len",
        }
    }

    pub(super) const fn code(self) -> u8 {
        self as u8
    }
}

impl fmt::Display for OpKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// A set of [`OpKind`]s. Sets are joined with `|`.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct OpSet(u16);

impl OpSet {
    /// No operation.
    pub const NONE: Self = Self(0);
    /// Every operation.
    pub const ALL: Self = Self(u16::MAX);
    /// The operations that only look: `read_at`, `len` and `read_dir`.
    pub const READS: Self = Self::of(&[OpKind::Read, OpKind::Len, OpKind::ReadDir]);
    /// The operations that change a file: `write_at`, `append` and `set_len`.
    pub const WRITES: Self = Self::of(&[OpKind::Write, OpKind::Append, OpKind::SetLen]);
    /// The operations that make changes durable: `sync_data`, `sync_all` and
    /// `sync_dir`.
    pub const SYNCS: Self = Self::of(&[OpKind::SyncData, OpKind::SyncAll, OpKind::SyncDir]);
    /// The operations that can change names: `open`, `rename`, `hard_link`,
    /// `remove_file`, `create_dir`, `remove_dir` and `lock`.
    pub const NAMES: Self = Self::of(&[
        OpKind::Open,
        OpKind::Rename,
        OpKind::HardLink,
        OpKind::RemoveFile,
        OpKind::CreateDir,
        OpKind::RemoveDir,
        OpKind::Lock,
    ]);

    /// The set of `kinds`.
    pub const fn of(kinds: &[OpKind]) -> Self {
        let mut bits = 0;
        let mut index = 0;
        while index < kinds.len() {
            bits |= 1 << kinds[index].code();
            index += 1;
        }
        Self(bits)
    }

    /// Whether `kind` is in the set.
    pub const fn contains(self, kind: OpKind) -> bool {
        self.0 & (1 << kind.code()) != 0
    }
}

impl BitOr for OpSet {
    type Output = Self;

    fn bitor(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
}

impl From<OpKind> for OpSet {
    fn from(kind: OpKind) -> Self {
        Self::of(&[kind])
    }
}

impl fmt::Debug for OpSet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_set()
            .entries(OpKind::ALL.into_iter().filter(|kind| self.contains(*kind)))
            .finish()
    }
}

/// An error the simulator injects.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fault {
    /// No space left on the device. The operation fails with
    /// [`StorageFull`](io::ErrorKind::StorageFull).
    ///
    /// Only operations that can need space get it: a `write_at` or an
    /// `append` of at least one byte, a `set_len` that grows the file, the
    /// three syncs, `rename`, `hard_link`, `create_dir`, and an `open` or a
    /// `lock` that would create its file.
    Enospc,
    /// The device failed. The operation fails with
    /// [`Other`](io::ErrorKind::Other), a kind the contract promises nothing
    /// about. Every operation can get it, except a read, a write or an
    /// append of no bytes.
    Eio,
}

impl fmt::Display for Fault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Enospc => "ENOSPC",
            Self::Eio => "EIO",
        })
    }
}

/// Faults injected by chance.
///
/// Whether operation number `n` gets one is a function of the seed, `n` and
/// these odds. A [`FaultRule`] that fires on an operation takes its place.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FaultOdds {
    /// How often an operation in [`eio_ops`](Self::eio_ops) fails with
    /// [`Fault::Eio`].
    pub eio: Chance,
    /// The operations that can fail with [`Fault::Eio`].
    pub eio_ops: OpSet,
    /// How often an operation in [`enospc_ops`](Self::enospc_ops) that can
    /// need space fails with [`Fault::Enospc`].
    pub enospc: Chance,
    /// The operations that can fail with [`Fault::Enospc`].
    pub enospc_ops: OpSet,
    /// How often an append that fails cannot be undone, which leaves the
    /// bytes it wrote in the file and breaks the handle for appending.
    pub torn_append: Chance,
}

impl FaultOdds {
    /// No faults.
    pub const NONE: Self = Self {
        eio: Chance::NEVER,
        eio_ops: OpSet::ALL,
        enospc: Chance::NEVER,
        enospc_ops: OpSet::ALL,
        torn_append: Chance::one_in(4),
    };
}

impl Default for FaultOdds {
    fn default() -> Self {
        Self::NONE
    }
}

/// A fault injected at chosen operations.
///
/// A rule looks at the operations that would otherwise succeed, are in
/// [`ops`](Self::ops), are numbered [`from_op`](Self::from_op) or later, are
/// on [`path`](Self::path) if one is set, and can get the fault at all (see
/// [`Fault::Enospc`]). It lets the first [`skip`](Self::skip) of them
/// through, fails the next [`times`](Self::times), and is then spent.
///
/// ```
/// use lignin_io::sim::{Fault, FaultRule, OpSet};
///
/// // Operation 57 fails with EIO, or the first one after it that would
/// // otherwise succeed.
/// FaultRule::new(Fault::Eio).from_op(57);
/// // The disk is full from operation 100 on, until the rule is cleared.
/// FaultRule::new(Fault::Enospc).from_op(100).forever();
/// // The third sync of one file fails.
/// FaultRule::new(Fault::Eio)
///     .on(OpSet::SYNCS)
///     .at("/wal/00000001.log")
///     .skip(2);
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FaultRule {
    /// The error.
    pub fault: Fault,
    /// The operations the rule looks at.
    pub ops: OpSet,
    /// The file or directory the operations must be on: the path a file was
    /// opened by, or the path given to the operation. For `rename` and
    /// `hard_link` either of the two paths counts.
    pub path: Option<PathBuf>,
    /// The number of the first operation the rule looks at.
    pub from_op: u64,
    /// How many operations the rule lets through first.
    pub skip: u64,
    /// How many operations the rule fails.
    pub times: u64,
    /// Whether a failed append is left in the file, which breaks the handle
    /// for appending. `None` leaves it to [`FaultOdds::torn_append`].
    pub torn_append: Option<bool>,
}

impl FaultRule {
    /// A rule that fails the next operation that can get `fault`.
    pub fn new(fault: Fault) -> Self {
        Self {
            fault,
            ops: OpSet::ALL,
            path: None,
            from_op: 0,
            skip: 0,
            times: 1,
            torn_append: None,
        }
    }

    /// Sets [`ops`](Self::ops).
    pub fn on(mut self, ops: impl Into<OpSet>) -> Self {
        self.ops = ops.into();
        self
    }

    /// Sets [`path`](Self::path).
    pub fn at(mut self, path: impl AsRef<Path>) -> Self {
        self.path = Some(path.as_ref().to_path_buf());
        self
    }

    /// Sets [`from_op`](Self::from_op).
    pub fn from_op(mut self, number: u64) -> Self {
        self.from_op = number;
        self
    }

    /// Sets [`skip`](Self::skip).
    pub fn skip(mut self, count: u64) -> Self {
        self.skip = count;
        self
    }

    /// Sets [`times`](Self::times).
    pub fn times(mut self, count: u64) -> Self {
        self.times = count;
        self
    }

    /// Makes the rule fail every operation it looks at until it is cleared.
    pub fn forever(self) -> Self {
        self.times(u64::MAX)
    }

    /// Sets [`torn_append`](Self::torn_append).
    pub fn torn_append(mut self, torn: bool) -> Self {
        self.torn_append = Some(torn);
        self
    }
}

/// Which handles a write-back that fails marks. The next sync through a
/// marked handle is told of the failure. A handle on which a sync has
/// failed is never marked: it syncs no more.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Told {
    /// As the error sequence of Linux tells them. Every handle of the file
    /// that is open when the write-back fails, and on which no sync has
    /// failed, is marked. So is a handle that is opened later, unless a
    /// sync was told of the failure before, a later failure of this kind
    /// took its place, or the file had no open handle at some moment since:
    /// the record is gone with the file's last handle, which Linux allows
    /// and does not always do.
    OpenHandles,
    /// The least the contract promises: the open handles that made one of
    /// the changes the write-back was to write, and on which no sync has
    /// failed. No other handle is marked, whenever it is opened.
    Writers,
}

/// A write-back that fails at a chosen moment: the system's own write of a
/// file's unsynced changes, which no call reports. The
/// [module documentation](super#a-write-back-that-fails) says what it
/// leaves.
///
/// A rule looks at the operations that are in [`ops`](Self::ops), are
/// numbered [`from_op`](Self::from_op) or later, are on
/// [`path`](Self::path) if one is set, and leave their file with unsynced
/// changes. It lets the first [`skip`](Self::skip) of them through. After
/// each of the next [`times`](Self::times) the write-back of the file fails,
/// and the rule is then spent. The operation itself returns what it would
/// have.
///
/// Only an operation on an open file is looked at: one that is made through
/// a handle, whether it succeeds or not, or an `open` that succeeds. A rule
/// counts operations, not changes: a `write_at` that is refused, and a
/// `set_len` to the length the file has, are looked at like any other while
/// the file has unsynced changes. The write-back that fails is the file's:
/// of every unsynced change of it, through whichever handle it was made.
///
/// ```
/// use lignin_io::sim::{Fault, OpKind, Told, WriteBackRule};
///
/// // The write-back of a file fails after the third `write_at`, `append`
/// // or `set_len` that leaves it with unsynced changes, through a handle
/// // that was opened by this path. The changes are forgotten at once.
/// WriteBackRule::new(Fault::Eio)
///     .at("/segments/00000007.seg")
///     .skip(2)
///     .forgotten(true);
/// // After the next `len` of any file with unsynced changes, and it marks
/// // as `Told::Writers` says.
/// WriteBackRule::new(Fault::Enospc)
///     .on(OpKind::Len)
///     .told(Told::Writers);
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WriteBackRule {
    /// The error a sync fails with that is told of the write-back.
    pub fault: Fault,
    /// The operations the rule looks at: unless set, `write_at`, `append`
    /// and `set_len`.
    pub ops: OpSet,
    /// The path the handle of an operation must have been opened by: for an
    /// `open`, the path it is given.
    pub path: Option<PathBuf>,
    /// The number of the first operation the rule looks at.
    pub from_op: u64,
    /// How many operations the rule lets through first.
    pub skip: u64,
    /// After how many operations the write-back fails.
    pub times: u64,
    /// Which handles are marked: unless set, [`Told::OpenHandles`].
    pub told: Told,
    /// Whether the system forgets at once what storage does not have of
    /// the file: the [module documentation](super#a-write-back-that-fails)
    /// says what operations see then. `None` leaves it to
    /// [`WriteBackOdds::forgotten`].
    pub forgotten: Option<bool>,
}

impl WriteBackRule {
    /// A rule that fails the write-back after the next `write_at`, `append`
    /// or `set_len` that leaves its file with unsynced changes. It marks as
    /// [`Told::OpenHandles`] says, and whether the changes are forgotten is
    /// left to [`WriteBackOdds::forgotten`]: one time in two unless the
    /// configuration says otherwise.
    pub fn new(fault: Fault) -> Self {
        Self {
            fault,
            ops: OpSet::WRITES,
            path: None,
            from_op: 0,
            skip: 0,
            times: 1,
            told: Told::OpenHandles,
            forgotten: None,
        }
    }

    /// Sets [`ops`](Self::ops).
    pub fn on(mut self, ops: impl Into<OpSet>) -> Self {
        self.ops = ops.into();
        self
    }

    /// Sets [`path`](Self::path).
    pub fn at(mut self, path: impl AsRef<Path>) -> Self {
        self.path = Some(path.as_ref().to_path_buf());
        self
    }

    /// Sets [`from_op`](Self::from_op).
    pub fn from_op(mut self, number: u64) -> Self {
        self.from_op = number;
        self
    }

    /// Sets [`skip`](Self::skip).
    pub fn skip(mut self, count: u64) -> Self {
        self.skip = count;
        self
    }

    /// Sets [`times`](Self::times).
    pub fn times(mut self, count: u64) -> Self {
        self.times = count;
        self
    }

    /// Makes the write-back fail after every operation the rule looks at,
    /// until the rule is cleared.
    pub fn forever(self) -> Self {
        self.times(u64::MAX)
    }

    /// Sets [`told`](Self::told).
    pub fn told(mut self, told: Told) -> Self {
        self.told = told;
        self
    }

    /// Sets [`forgotten`](Self::forgotten).
    pub fn forgotten(mut self, forgotten: bool) -> Self {
        self.forgotten = Some(forgotten);
        self
    }
}

/// Write-backs that fail by chance.
///
/// After operation number `n`, if it is one of [`ops`](Self::ops) on an
/// open file and leaves that file with unsynced changes, whether the
/// write-back fails is a function of the seed, `n` and these odds. A
/// [`WriteBackRule`] whose turn it is after an operation takes their place.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WriteBackOdds {
    /// How often the write-back fails after an operation in
    /// [`ops`](Self::ops) that leaves its file with unsynced changes.
    pub fail: Chance,
    /// The operations after which it can fail.
    pub ops: OpSet,
    /// The error a sync fails with that is told of it.
    pub fault: Fault,
    /// Which handles are marked.
    pub told: Told,
    /// How often the system forgets at once what storage does not have of
    /// the file. This also decides for a [`WriteBackRule`] that leaves
    /// [`forgotten`](WriteBackRule::forgotten) open.
    pub forgotten: Chance,
}

impl WriteBackOdds {
    /// No write-back fails by chance. Where a [`WriteBackRule`] fails one
    /// and does not say whether the changes are forgotten, they are one
    /// time in two.
    pub const NONE: Self = Self {
        fail: Chance::NEVER,
        ops: OpSet::WRITES,
        fault: Fault::Eio,
        told: Told::OpenHandles,
        forgotten: Chance::one_in(2),
    };
}

impl Default for WriteBackOdds {
    fn default() -> Self {
        Self::NONE
    }
}

/// What an [`io::Error`] made by the simulator carries.
///
/// [`SimError::of`] finds it in an error.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SimError {
    /// The simulator injected this failure: into the operation, or into a
    /// write-back that the operation, a sync, is the one to report.
    Injected {
        /// The error that was injected.
        fault: Fault,
        /// The number of the operation that failed.
        op: u64,
        /// The kind of the operation.
        kind: OpKind,
        /// The path the operation was on.
        path: String,
    },
    /// The power is off: the simulated machine stopped at its crash point and
    /// does nothing more.
    PowerLoss,
}

impl SimError {
    /// The simulator's part of `error`, if the simulator made it by injecting
    /// a fault or because the power is off.
    pub fn of(error: &io::Error) -> Option<&Self> {
        error.get_ref()?.downcast_ref()
    }

    /// Whether `error` is the one every operation gets once the power is off.
    pub fn is_power_loss(error: &io::Error) -> bool {
        matches!(Self::of(error), Some(Self::PowerLoss))
    }

    /// The fault that was injected, if `error` is an injected one.
    pub fn injected(error: &io::Error) -> Option<Fault> {
        match Self::of(error) {
            Some(Self::Injected { fault, .. }) => Some(*fault),
            _ => None,
        }
    }
}

impl fmt::Display for SimError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Injected {
                fault,
                op,
                kind,
                path,
            } => write!(f, "simulated {fault} in operation {op} ({kind} {path})"),
            Self::PowerLoss => f.write_str("the simulated power is off"),
        }
    }
}

impl Error for SimError {}

impl From<SimError> for io::Error {
    fn from(error: SimError) -> Self {
        let kind = match error {
            SimError::Injected {
                fault: Fault::Enospc,
                ..
            } => io::ErrorKind::StorageFull,
            _ => io::ErrorKind::Other,
        };
        Self::new(kind, error)
    }
}
