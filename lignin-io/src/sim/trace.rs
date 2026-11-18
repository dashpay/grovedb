//! What the simulator did, as a record a failing seed can be understood
//! from.

use std::{fmt, io};

use super::{
    crash::{FileChangeKind, NameChange, Tear},
    fault::{Fault, OpKind},
};

/// One entry of [`SimVfs::trace`](super::SimVfs::trace).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// An operation and how it ended. An operation a fault was injected into
    /// is always recorded, every other one only with
    /// [`SimConfig::trace_ops`](super::SimConfig::trace_ops).
    Op(OpRecord),
    /// A sync succeeded on a file that an earlier, failed sync had left with
    /// unknown changes. This is what became of them, drawn from the seed: the
    /// sync itself covers only the changes made after the failure.
    Settled {
        /// The number of the sync that succeeded.
        op: u64,
        /// The path the file was synced by.
        path: String,
        /// The changes the failed sync left unknown, in the order they were
        /// made.
        changes: Vec<ChangeFate>,
    },
    /// The power went off after this operation: the file system had reached
    /// its [`CrashPoint`](super::CrashPoint).
    PowerOff {
        /// The number of the last operation that took place.
        after_op: u64,
    },
    /// This file system is what a crash left of another one. It is the first
    /// event of a file system made by
    /// [`CrashImage::recover`](super::CrashImage::recover).
    Crash(CrashRecord),
}

/// An operation and how it ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpRecord {
    /// The operation's number, counted from 1 in its file system.
    pub op: u64,
    /// The operation.
    pub kind: OpKind,
    /// The path it was given, or the path its file was opened by.
    pub path: String,
    /// The second path of a `rename` or a `hard_link`.
    pub to: Option<String>,
    /// The offset of a `read_at`, a `write_at` or an `append`.
    pub offset: u64,
    /// The length of the buffer, or the length given to `set_len`.
    pub len: u64,
    /// How it ended.
    pub outcome: Outcome,
}

/// How an operation ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// It succeeded.
    Ok,
    /// It failed as the contract says it must, for example with `NotFound`.
    Failed(io::ErrorKind),
    /// The simulator made it fail.
    Injected {
        /// The error it failed with.
        fault: Fault,
        /// What the failed operation left behind.
        effect: Effect,
    },
}

/// What an operation the simulator failed left behind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Effect {
    /// Nothing changed.
    Nothing,
    /// A `write_at` wrote this many bytes of its buffer before it failed, in
    /// whole sectors that need not be its first.
    Wrote(u64),
    /// An `append` wrote this many bytes, failed, and took them back: the
    /// file has the length it had. A crash can still bring them back.
    AppendUndone(u64),
    /// An `append` wrote this many bytes, failed, and could not take them
    /// back: they stay in the file, and the handle appends no more.
    AppendTorn(u64),
    /// A sync failed and left this many unsynced changes unknown: no later
    /// sync makes them durable.
    SyncFailed(u64),
}

/// What a crash kept of a file system.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CrashRecord {
    /// The seed of the file system that crashed.
    pub seed: u64,
    /// How many operations it had made.
    pub ops: u64,
    /// Its unsynced changes of names, in the order they were made.
    pub names: Vec<NameFate>,
    /// Its unsynced changes of files, in the order they were made.
    pub changes: Vec<ChangeFate>,
}

/// What became of an unsynced change of names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NameFate {
    /// The change.
    pub change: NameChange,
    /// Whether it survived.
    pub survives: bool,
}

/// What became of an unsynced change of a file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChangeFate {
    /// The number of the operation that made the change.
    pub op: u64,
    /// The path its file was opened by.
    pub path: String,
    /// The change.
    pub kind: FileChangeKind,
    /// What became of it.
    pub fate: Fate,
}

/// What became of an unsynced change of a file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Fate {
    /// It was lost.
    Lost,
    /// It survived.
    Survives {
        /// Its place among the surviving changes, counted from 0: they were
        /// applied in this order, which need not be the order they were made
        /// in.
        position: u64,
        /// The part of a write that survived, if not all of it did.
        tear: Option<Tear>,
    },
}

impl fmt::Display for Event {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Op(record) => record.fmt(f),
            Self::Settled { op, path, changes } => {
                write!(
                    f,
                    "#{op} the sync of {path} settles what a failed sync left unknown:"
                )?;
                changes
                    .iter()
                    .try_for_each(|change| write!(f, "\n  {change}"))
            }
            Self::PowerOff { after_op } => write!(f, "power off after #{after_op}"),
            Self::Crash(record) => record.fmt(f),
        }
    }
}

impl fmt::Display for OpRecord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{} {} {}", self.op, self.kind, self.path)?;
        if let Some(to) = &self.to {
            write!(f, " -> {to}")?;
        }
        match self.kind {
            OpKind::Read | OpKind::Write | OpKind::Append => {
                write!(f, " offset {} len {}", self.offset, self.len)?;
            }
            OpKind::SetLen => write!(f, " len {}", self.len)?,
            _ => {}
        }
        write!(f, ": {}", self.outcome)
    }
}

impl fmt::Display for Outcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ok => f.write_str("ok"),
            Self::Failed(kind) => write!(f, "failed ({kind:?})"),
            Self::Injected { fault, effect } => write!(f, "injected {fault}{effect}"),
        }
    }
}

impl fmt::Display for Effect {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Nothing => Ok(()),
            Self::Wrote(bytes) => write!(f, ", {bytes} bytes were written"),
            Self::AppendUndone(bytes) => {
                write!(f, ", {bytes} bytes were appended and taken back")
            }
            Self::AppendTorn(bytes) => {
                write!(f, ", {bytes} bytes stay appended and the handle is broken")
            }
            Self::SyncFailed(changes) => write!(f, ", {changes} unsynced changes left unknown"),
        }
    }
}

impl fmt::Display for CrashRecord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "crash of the file system with seed {:#018x} after #{}:",
            self.seed, self.ops
        )?;
        for name in &self.names {
            write!(f, "\n  {name}")?;
        }
        self.changes
            .iter()
            .try_for_each(|change| write!(f, "\n  {change}"))
    }
}

impl fmt::Display for NameFate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let fate = if self.survives { "survives" } else { "lost" };
        write!(f, "{}: {fate}", self.change)
    }
}

impl fmt::Display for ChangeFate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{} {} {}: {}", self.op, self.kind, self.path, self.fate)
    }
}

impl fmt::Display for Fate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Lost => f.write_str("lost"),
            Self::Survives {
                position,
                tear: None,
            } => write!(f, "survives whole, applied at {position}"),
            Self::Survives {
                position,
                tear: Some(tear),
            } => write!(f, "survives torn ({tear}), applied at {position}"),
        }
    }
}

/// A 64-bit digest of `events`: FNV-1a over a fixed encoding of them, the
/// same on every machine. Two runs whose traces have different digests did
/// not make the same schedule.
pub fn digest(events: &[Event]) -> u64 {
    let mut hasher = Fnv::new();
    for event in events {
        event.hash(&mut hasher);
    }
    hasher.0
}

pub(super) struct Fnv(pub(super) u64);

impl Fnv {
    pub(super) fn new() -> Self {
        Self(0xCBF2_9CE4_8422_2325)
    }

    pub(super) fn bytes(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            self.0 = (self.0 ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01B3);
        }
    }

    pub(super) fn u64(&mut self, value: u64) {
        self.bytes(&value.to_le_bytes());
    }

    pub(super) fn text(&mut self, text: &str) {
        self.u64(text.len() as u64);
        self.bytes(text.as_bytes());
    }

    fn optional_text(&mut self, text: Option<&str>) {
        match text {
            None => self.u64(0),
            Some(text) => {
                self.u64(1);
                self.text(text);
            }
        }
    }
}

impl Event {
    fn hash(&self, hasher: &mut Fnv) {
        match self {
            Self::Op(record) => {
                hasher.u64(1);
                hasher.u64(record.op);
                hasher.u64(record.kind.code().into());
                hasher.text(&record.path);
                hasher.optional_text(record.to.as_deref());
                hasher.u64(record.offset);
                hasher.u64(record.len);
                record.outcome.hash(hasher);
            }
            Self::Settled { op, path, changes } => {
                hasher.u64(2);
                hasher.u64(*op);
                hasher.text(path);
                hash_changes(changes, hasher);
            }
            Self::PowerOff { after_op } => {
                hasher.u64(3);
                hasher.u64(*after_op);
            }
            Self::Crash(record) => {
                hasher.u64(4);
                hasher.u64(record.seed);
                hasher.u64(record.ops);
                hasher.u64(record.names.len() as u64);
                for name in &record.names {
                    hasher.u64(name.change.op);
                    hasher.u64(name.change.kind as u64);
                    hasher.text(&name.change.path);
                    hasher.optional_text(name.change.to.as_deref());
                    hasher.u64(name.change.file.map_or(0, |file| file + 1));
                    hasher.u64(name.survives.into());
                }
                hash_changes(&record.changes, hasher);
            }
        }
    }
}

impl Outcome {
    fn hash(&self, hasher: &mut Fnv) {
        match self {
            Self::Ok => hasher.u64(0),
            Self::Failed(kind) => {
                hasher.u64(1);
                hasher.u64(error_code(*kind));
            }
            Self::Injected { fault, effect } => {
                hasher.u64(2);
                hasher.u64(*fault as u64);
                let (code, count) = match effect {
                    Effect::Nothing => (0, 0),
                    Effect::Wrote(bytes) => (1, *bytes),
                    Effect::AppendUndone(bytes) => (2, *bytes),
                    Effect::AppendTorn(bytes) => (3, *bytes),
                    Effect::SyncFailed(changes) => (4, *changes),
                };
                hasher.u64(code);
                hasher.u64(count);
            }
        }
    }
}

fn hash_changes(changes: &[ChangeFate], hasher: &mut Fnv) {
    hasher.u64(changes.len() as u64);
    for change in changes {
        hasher.u64(change.op);
        hasher.text(&change.path);
        match change.kind {
            FileChangeKind::Write { offset, len } => {
                hasher.u64(1);
                hasher.u64(offset);
                hasher.u64(len);
            }
            FileChangeKind::SetLen { len } => {
                hasher.u64(2);
                hasher.u64(len);
            }
        }
        match &change.fate {
            Fate::Lost => hasher.u64(0),
            Fate::Survives { position, tear } => {
                hasher.u64(1);
                hasher.u64(*position);
                match tear {
                    None => hasher.u64(0),
                    Some(tear) => {
                        hasher.u64(1);
                        hasher.u64(tear.len);
                        hasher.u64(tear.sectors.len() as u64);
                        for sectors in &tear.sectors {
                            hasher.u64(sectors.start);
                            hasher.u64(sectors.end);
                        }
                    }
                }
            }
        }
    }
}

fn error_code(kind: io::ErrorKind) -> u64 {
    use io::ErrorKind::*;
    match kind {
        NotFound => 1,
        AlreadyExists => 2,
        ResourceBusy => 3,
        IsADirectory => 4,
        NotADirectory => 5,
        DirectoryNotEmpty => 6,
        WouldBlock => 7,
        UnexpectedEof => 8,
        InvalidInput => 9,
        FileTooLarge => 10,
        PermissionDenied => 11,
        Other => 12,
        _ => u64::MAX,
    }
}
