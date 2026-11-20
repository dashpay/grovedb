//! The simulated file system: one value behind one lock, in which every
//! operation runs from start to end.

use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::{OsStr, OsString},
    io::{self, ErrorKind},
    path::{Component, Path},
    sync::Arc,
};

use super::{
    crash::{
        self, Change, CrashImage, FileChange, FileChangeKind, NameChange, NameChangeKind, Survivor,
    },
    fault::{Fault, FaultRule, OpKind, OpSet, SimError},
    rng::{stream, SimRng},
    trace::{Effect, Event, Fnv, OpRecord, Outcome},
    CrashPoint, SimConfig, SimStats, UnknownFate,
};
use crate::vfs::{Create, DirEntry, EntryKind, OpenMode};

pub(super) const ROOT: DirId = DirId(0);

/// The longest file a 32-bit machine can hold in memory. No file may be
/// longer on any machine, so that the same calls end the same way on all.
pub(super) const MAX_FILE_LEN: u64 = i32::MAX as u64;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct DirId(u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct FileId(pub(super) u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Node {
    File(FileId),
    Dir(DirId),
}

/// One name in one directory. `now` is what operations see. A crash sees
/// `durable`, or the last of the `pending` changes it keeps. A change is known
/// by the number of the operation that made it, and a crash that keeps one
/// keeps the ones before it.
#[derive(Clone, Debug, Default)]
pub(super) struct Cell {
    now: Option<Node>,
    pub(super) durable: Option<Node>,
    pub(super) pending: Vec<(u64, Option<Node>)>,
}

impl Cell {
    /// The numbers of the unsynced changes of the name, rising.
    fn unsynced(&self) -> impl Iterator<Item = u64> + '_ {
        self.pending.iter().map(|(seq, _)| *seq)
    }

    fn nodes(&self) -> impl Iterator<Item = Node> + '_ {
        let pending = self.pending.iter().filter_map(|(_, node)| *node);
        self.now.into_iter().chain(self.durable).chain(pending)
    }
}

/// A directory, with the name operations see it under in the directory above
/// it: the one it was made with, or the one its last rename gave it.
#[derive(Clone, Debug)]
pub(super) struct Dir {
    pub(super) cells: BTreeMap<OsString, Cell>,
    refs: u32,
    name: Option<(DirId, OsString)>,
}

/// `content` is what operations see. A crash sees `durable` with some of the
/// `pending` changes. The first `unknown` of them were left unknown by a
/// failed sync, and no later sync makes them durable.
#[derive(Debug)]
struct File {
    content: Arc<Vec<u8>>,
    durable: Arc<Vec<u8>>,
    pending: Vec<Change>,
    unknown: usize,
    appender: bool,
    writers: u32,
    refs: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Access {
    Read,
    Write,
    Append,
}

#[derive(Debug)]
struct Handle {
    file: FileId,
    access: Access,
    path: Arc<str>,
    sync_failed: bool,
    append_broken: bool,
}

/// A change of names that is not durable yet. It becomes durable once every
/// directory in `waiting` was synced, or with a later change that builds on
/// it.
///
/// `removed` is the directory the change took away: the one a `remove_dir`
/// removed, or the empty one a rename replaced. `placed` is for a rename:
/// the changes, unsynced when it was made, that had put the directories its
/// two names are in, and every directory above them, under the names they
/// had then.
#[derive(Clone, Debug)]
pub(super) struct NameOp {
    kind: NameChangeKind,
    path: Arc<str>,
    to: Option<Arc<str>>,
    file: Option<FileId>,
    removed: Option<DirId>,
    placed: Vec<u64>,
    waiting: Vec<DirId>,
    cells: Vec<(DirId, OsString)>,
}

/// The unsynced changes of names in `kept`, with every change they build on.
///
/// This is the rule of the contract. A change builds on the earlier changes
/// of the names it touches. The removal of a directory, and a rename that
/// replaces an empty one, builds on the changes that emptied it. A rename
/// builds on what gave the directories its two names are in, and every
/// directory above them, the names they had when it was made: the creation
/// of each, or its last rename. Without the changes it builds on, a kept
/// change would leave a rename half done: a later change of one of its names
/// would act as if the file had moved, while the other name showed that it
/// had not. And a rename kept without the one that had moved a directory
/// above it could put a directory into itself.
///
/// With them, every kept rename finds its two names, and the directories
/// above them up to the root, as it found them when it was made. So it moves
/// what it moved then, to where it moved it then: no directory goes into
/// itself or gets a second name, and what the kept changes leave is a tree.
/// A kept change of another kind, in a directory whose own name is lost, is
/// lost with that directory.
pub(super) fn with_what_they_build_on(
    dirs: &BTreeMap<DirId, Dir>,
    ops: &BTreeMap<u64, NameOp>,
    kept: impl IntoIterator<Item = u64>,
) -> BTreeSet<u64> {
    let mut closed = BTreeSet::new();
    let mut queue: Vec<u64> = kept.into_iter().collect();
    while let Some(seq) = queue.pop() {
        if !closed.insert(seq) {
            continue;
        }
        let op = &ops[&seq];
        for (dir, name) in &op.cells {
            let unsynced = dirs[dir].cells[name].unsynced();
            queue.extend(unsynced.take_while(|earlier| *earlier < seq));
        }
        if let Some(removed) = op.removed {
            queue.extend(dirs[&removed].cells.values().flat_map(Cell::unsynced));
        }
        let unsynced = |placed: &&u64| ops.contains_key(placed);
        queue.extend(op.placed.iter().filter(unsynced));
    }
    closed
}

#[derive(Debug)]
pub(super) struct ArmedRule {
    rule: FaultRule,
    path: Option<Arc<str>>,
    seen: u64,
}

impl ArmedRule {
    pub(super) fn new(rule: FaultRule) -> Self {
        let path = rule.path.as_deref().map(canonical);
        Self {
            rule,
            path,
            seen: 0,
        }
    }
}

/// The path of an operation, and the second path of a rename or a link.
type Paths = (Arc<str>, Option<Arc<str>>);

struct Op {
    number: u64,
    kind: OpKind,
    path: Arc<str>,
    to: Option<Arc<str>>,
    offset: u64,
    len: u64,
    changed: bool,
    injected: Option<(Fault, Effect)>,
}

impl Op {
    fn failure(&self, fault: Fault) -> io::Error {
        SimError::Injected {
            fault,
            op: self.number,
            kind: self.kind,
            path: self.path.to_string(),
        }
        .into()
    }

    fn failure_with(&mut self, fault: Fault, effect: Effect) -> io::Error {
        self.injected = Some((fault, effect));
        self.failure(fault)
    }
}

struct Injection {
    fault: Fault,
    rng: SimRng,
    torn_append: Option<bool>,
}

#[derive(Debug)]
pub(super) struct State {
    pub(super) config: SimConfig,
    pub(super) rules: Vec<ArmedRule>,
    pub(super) stats: SimStats,
    pub(super) powered_off: bool,
    pub(super) crash_point: Option<CrashPoint>,
    pub(super) trace: Vec<Event>,
    next_id: u64,
    dirs: BTreeMap<DirId, Dir>,
    files: BTreeMap<FileId, File>,
    handles: BTreeMap<u64, Handle>,
    locks: BTreeSet<FileId>,
    name_ops: BTreeMap<u64, NameOp>,
}

fn components(path: &Path) -> io::Result<Vec<&OsStr>> {
    let mut names = Vec::new();
    for component in path.components() {
        match component {
            Component::RootDir | Component::CurDir => {}
            Component::Normal(name) => names.push(name),
            Component::ParentDir | Component::Prefix(_) => {
                return Err(refuse(
                    ErrorKind::InvalidInput,
                    &path.to_string_lossy(),
                    "has a `..` or a prefix, which the simulator does not take",
                ));
            }
        }
    }
    Ok(names)
}

/// The path as the trace and the fault rules know it: `/` and the names,
/// joined by `/`.
pub(super) fn canonical(path: &Path) -> Arc<str> {
    let Ok(names) = components(path) else {
        return path.to_string_lossy().into();
    };
    if names.is_empty() {
        return "/".into();
    }
    let mut text = String::new();
    for name in names {
        text.push('/');
        text.push_str(&name.to_string_lossy());
    }
    text.into()
}

fn refuse(kind: ErrorKind, path: &str, problem: &str) -> io::Error {
    io::Error::new(kind, format!("{path} {problem}"))
}

impl State {
    pub(super) fn new(config: SimConfig) -> Self {
        assert!(config.sector_size > 0, "a sector has at least one byte");
        assert!(
            config.max_file_len <= MAX_FILE_LEN,
            "the simulator holds files of up to 2 GiB"
        );
        let root = Dir {
            cells: BTreeMap::new(),
            refs: 1,
            name: None,
        };
        Self {
            config,
            rules: Vec::new(),
            stats: SimStats::default(),
            powered_off: false,
            crash_point: None,
            trace: Vec::new(),
            next_id: 1,
            dirs: BTreeMap::from([(ROOT, root)]),
            files: BTreeMap::new(),
            handles: BTreeMap::new(),
            locks: BTreeSet::new(),
            name_ops: BTreeMap::new(),
        }
    }

    fn id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id - 1
    }

    pub(super) fn add_file(&mut self, content: Arc<Vec<u8>>) -> FileId {
        let id = FileId(self.id());
        let file = File {
            durable: content.clone(),
            content,
            pending: Vec::new(),
            unknown: 0,
            appender: false,
            writers: 0,
            refs: 0,
        };
        self.files.insert(id, file);
        id
    }

    /// A directory that is to be named `name` in `above`.
    pub(super) fn add_dir(&mut self, above: DirId, name: &OsStr) -> DirId {
        let id = DirId(self.id());
        let dir = Dir {
            cells: BTreeMap::new(),
            refs: 0,
            name: Some((above, name.to_owned())),
        };
        self.dirs.insert(id, dir);
        id
    }

    /// Gives `dir` the durable name `name` for `node`.
    pub(super) fn place(&mut self, dir: DirId, name: &OsStr, node: Node) {
        self.retain(node);
        self.retain(node);
        let cell = Cell {
            now: Some(node),
            durable: Some(node),
            pending: Vec::new(),
        };
        self.dir_mut(dir).cells.insert(name.to_owned(), cell);
    }

    fn dir_mut(&mut self, dir: DirId) -> &mut Dir {
        self.dirs.get_mut(&dir).expect("a directory in use exists")
    }

    fn file_mut(&mut self, file: FileId) -> &mut File {
        self.files.get_mut(&file).expect("a file in use exists")
    }

    fn handle_mut(&mut self, handle: u64) -> &mut Handle {
        self.handles
            .get_mut(&handle)
            .expect("an open handle exists")
    }

    fn retain(&mut self, node: Node) {
        match node {
            Node::File(file) => self.file_mut(file).refs += 1,
            Node::Dir(dir) => self.dir_mut(dir).refs += 1,
        }
    }

    fn release(&mut self, node: Node) {
        match node {
            Node::File(id) => {
                let file = self.file_mut(id);
                file.refs -= 1;
                if file.refs == 0 {
                    self.files.remove(&id);
                }
            }
            Node::Dir(id) => {
                let dir = self.dir_mut(id);
                dir.refs -= 1;
                if dir.refs == 0 {
                    let gone = self.dirs.remove(&id);
                    debug_assert!(gone.is_some_and(|dir| dir.cells.is_empty()));
                }
            }
        }
    }

    fn now(&self, dir: DirId, name: &OsStr) -> Option<Node> {
        self.dirs[&dir].cells.get(name)?.now
    }

    fn dir_at(&self, names: &[&OsStr], shown: &str) -> io::Result<DirId> {
        let mut dir = ROOT;
        for name in names {
            dir = match self.now(dir, name) {
                Some(Node::Dir(dir)) => dir,
                Some(Node::File(_)) => {
                    return Err(refuse(
                        ErrorKind::NotADirectory,
                        shown,
                        "names a file where a directory is needed",
                    ));
                }
                None => {
                    return Err(refuse(
                        ErrorKind::NotFound,
                        shown,
                        "names a directory that does not exist",
                    ));
                }
            };
        }
        Ok(dir)
    }

    fn parent(&self, path: &Path, shown: &str, root: ErrorKind) -> io::Result<(DirId, OsString)> {
        let names = components(path)?;
        let Some((name, dirs)) = names.split_last() else {
            return Err(refuse(root, shown, "is the root directory"));
        };
        Ok((self.dir_at(dirs, shown)?, (*name).to_owned()))
    }

    fn bind(&mut self, dir: DirId, name: &OsStr, node: Option<Node>, seq: u64) {
        if let Some(node) = node {
            self.retain(node);
            self.retain(node);
        }
        let cell = self.dir_mut(dir).cells.entry(name.to_owned()).or_default();
        cell.pending.push((seq, node));
        if let Some(old) = std::mem::replace(&mut cell.now, node) {
            self.release(old);
        }
    }

    /// The directories above `dir`, nearest first, each with the name that
    /// the directory below it has in it. The directories form a tree, so the
    /// walk ends at the root. A state in which one of them is above itself is
    /// a fault of the simulator, and the walk panics there instead of going
    /// round for ever.
    fn above(&self, dir: DirId) -> impl Iterator<Item = &(DirId, OsString)> + '_ {
        let name_of = move |dir: DirId| self.dirs[&dir].name.as_ref();
        std::iter::successors(name_of(dir), move |(above, _)| name_of(*above))
            .enumerate()
            .map(move |(steps, at)| {
                assert!(
                    steps < self.dirs.len(),
                    "the simulator's directories form a cycle: the directory named {:?} is \
                     above itself",
                    at.1
                );
                at
            })
    }

    /// The unsynced changes that put `dir`, and every directory above it,
    /// under the names they have now.
    fn placed(&self, dir: DirId, placed: &mut Vec<u64>) {
        for (above, name) in self.above(dir) {
            placed.extend(self.dirs[above].cells[name].unsynced().last());
        }
    }

    /// Whether `dir` is `in_dir` or a directory below it.
    fn is_within(&self, dir: DirId, in_dir: DirId) -> bool {
        dir == in_dir || self.above(dir).any(|(above, _)| *above == in_dir)
    }

    /// Puts a change of names on record. `named` is the file or the
    /// directory whose name it changed, and `removed` the directory it took
    /// away, if it did.
    fn name_op(
        &mut self,
        op: &mut Op,
        kind: NameChangeKind,
        named: Node,
        removed: Option<DirId>,
        cells: Vec<(DirId, OsString)>,
    ) {
        let mut waiting: Vec<DirId> = cells.iter().map(|(dir, _)| *dir).collect();
        waiting.dedup();
        let file = match named {
            Node::File(file) => Some(file),
            Node::Dir(_) => None,
        };
        let mut placed = Vec::new();
        if kind == NameChangeKind::Rename {
            for dir in &waiting {
                self.placed(*dir, &mut placed);
            }
        }
        let name_op = NameOp {
            kind,
            path: op.path.clone(),
            to: op.to.clone(),
            file,
            removed,
            placed,
            waiting,
            cells,
        };
        self.name_ops.insert(op.number, name_op);
        op.changed = true;
    }

    /// The file to open or to lock: the one that is there, or a new one. Only
    /// a new one needs space.
    fn file_or_create(
        &mut self,
        existing: Option<FileId>,
        dir: DirId,
        name: OsString,
        op: &mut Op,
    ) -> io::Result<FileId> {
        self.fail_point(op, existing.is_none())?;
        if let Some(file) = existing {
            return Ok(file);
        }
        let file = self.add_file(Arc::default());
        self.bind(dir, &name, Some(Node::File(file)), op.number);
        let cells = vec![(dir, name)];
        self.name_op(op, NameChangeKind::Create, Node::File(file), None, cells);
        Ok(file)
    }

    /// Makes the changes of names in `dir` durable, with the ones they build
    /// on. A rename waits for the directories of both its names.
    fn settle_dir(&mut self, dir: DirId) -> bool {
        let cells = self.dirs[&dir].cells.values();
        let unsynced: BTreeSet<u64> = cells.flat_map(Cell::unsynced).collect();
        let mut synced = Vec::new();
        for seq in unsynced {
            let waiting = &mut self.name_op_mut(seq).waiting;
            waiting.retain(|waited| *waited != dir);
            if waiting.is_empty() {
                synced.push(seq);
            }
        }
        let settled = with_what_they_build_on(&self.dirs, &self.name_ops, synced);
        for seq in &settled {
            let op = self.name_ops.remove(seq).expect("the change is on record");
            for (cell_dir, name) in op.cells {
                self.settle_cell(cell_dir, &name);
            }
        }
        !settled.is_empty()
    }

    fn name_op_mut(&mut self, seq: u64) -> &mut NameOp {
        self.name_ops
            .get_mut(&seq)
            .expect("an unsynced change of names is on record")
    }

    /// Makes the oldest unsynced change of a name durable.
    fn settle_cell(&mut self, dir: DirId, name: &OsStr) {
        let cells = &mut self.dir_mut(dir).cells;
        let cell = cells.get_mut(name).expect("a changed name is on record");
        let (_, node) = cell.pending.remove(0);
        let old = std::mem::replace(&mut cell.durable, node);
        if cell.now.is_none() && cell.durable.is_none() && cell.pending.is_empty() {
            cells.remove(name);
        }
        if let Some(old) = old {
            self.release(old);
        }
    }

    fn run<T>(
        &mut self,
        kind: OpKind,
        (path, to): Paths,
        (offset, len): (u64, u64),
        body: impl FnOnce(&mut Self, &mut Op) -> io::Result<T>,
    ) -> io::Result<T> {
        if self.powered_off {
            return Err(SimError::PowerLoss.into());
        }
        self.stats.ops += 1;
        if OpSet::SYNCS.contains(kind) {
            self.stats.syncs += 1;
        }
        let mut op = Op {
            number: self.stats.ops,
            kind,
            path,
            to,
            offset,
            len,
            changed: false,
            injected: None,
        };
        let result = body(self, &mut op);
        if op.changed {
            self.stats.changes += 1;
        }
        let outcome = match (&result, op.injected) {
            (_, Some((fault, effect))) => Outcome::Injected { fault, effect },
            (Ok(_), None) => Outcome::Ok,
            (Err(error), None) => Outcome::Failed(error.kind()),
        };
        if self.config.trace_ops || op.injected.is_some() {
            self.trace.push(Event::Op(OpRecord {
                op: op.number,
                kind,
                path: op.path.to_string(),
                to: op.to.as_deref().map(str::to_owned),
                offset: op.offset,
                len: op.len,
                outcome,
            }));
        }
        self.check_crash_point();
        result
    }

    pub(super) fn check_crash_point(&mut self) {
        let reached = match self.crash_point {
            None => false,
            Some(CrashPoint::AfterOp(count)) => self.stats.ops >= count,
            Some(CrashPoint::AfterSync(count)) => self.stats.syncs >= count,
            Some(CrashPoint::AfterChange(count)) => self.stats.changes >= count,
        };
        if reached {
            self.power_off();
        }
    }

    pub(super) fn power_off(&mut self) {
        if !self.powered_off {
            self.powered_off = true;
            self.crash_point = None;
            let after_op = self.stats.ops;
            self.trace.push(Event::PowerOff { after_op });
        }
    }

    /// Decides whether the operation, which would otherwise succeed, fails.
    fn fault(&mut self, op: &mut Op, needs_space: bool) -> Option<Injection> {
        let mut hit = None;
        for armed in &mut self.rules {
            let rule = &armed.rule;
            let named = |path: &Arc<str>| *path == op.path || op.to.as_ref() == Some(path);
            if !armed.path.as_ref().is_none_or(named)
                || !rule.ops.contains(op.kind)
                || op.number < rule.from_op
                || (rule.fault == Fault::Enospc && !needs_space)
            {
                continue;
            }
            armed.seen += 1;
            if hit.is_none() && armed.seen > rule.skip && armed.seen - rule.skip <= rule.times {
                hit = Some((rule.fault, rule.torn_append));
            }
        }
        if hit.is_none() {
            let odds = &self.config.faults;
            let mut rng = SimRng::derive(self.config.seed, stream::FAULT, op.number);
            let can_be_full = needs_space && odds.enospc_ops.contains(op.kind);
            if odds.eio_ops.contains(op.kind) && rng.chance(odds.eio) {
                hit = Some((Fault::Eio, None));
            } else if can_be_full && rng.chance(odds.enospc) {
                hit = Some((Fault::Enospc, None));
            }
        }
        let (fault, torn_append) = hit?;
        self.stats.faults += 1;
        op.injected = Some((fault, Effect::Nothing));
        Some(Injection {
            fault,
            rng: SimRng::derive(self.config.seed, stream::DETAIL, op.number),
            torn_append,
        })
    }

    fn fail_point(&mut self, op: &mut Op, needs_space: bool) -> io::Result<()> {
        match self.fault(op, needs_space) {
            Some(injection) => Err(op.failure(injection.fault)),
            None => Ok(()),
        }
    }

    pub(super) fn open(&mut self, path: &Path, mode: OpenMode) -> io::Result<u64> {
        let paths = (canonical(path), None);
        self.run(OpKind::Open, paths, (0, 0), |state, op| {
            let (access, create) = match mode {
                OpenMode::Read => (Access::Read, Create::Never),
                OpenMode::Write(create) => (Access::Write, create),
                OpenMode::Append(create) => (Access::Append, create),
            };
            let (dir, name) = state.parent(path, &op.path, ErrorKind::IsADirectory)?;
            let existing = match state.now(dir, &name) {
                Some(_) if create == Create::New => {
                    return Err(refuse(ErrorKind::AlreadyExists, &op.path, "already exists"));
                }
                Some(Node::Dir(_)) => {
                    return Err(refuse(ErrorKind::IsADirectory, &op.path, "is a directory"));
                }
                Some(Node::File(file)) => {
                    let open = &state.files[&file];
                    let busy = match access {
                        Access::Read => false,
                        Access::Write => open.appender,
                        Access::Append => open.appender || open.writers > 0,
                    };
                    if busy {
                        return Err(refuse(
                            ErrorKind::ResourceBusy,
                            &op.path,
                            "has another writer",
                        ));
                    }
                    Some(file)
                }
                None if create == Create::Never => {
                    return Err(refuse(ErrorKind::NotFound, &op.path, "does not exist"));
                }
                None => None,
            };
            let file = state.file_or_create(existing, dir, name, op)?;
            let handle = state.id();
            let open = state.file_mut(file);
            open.refs += 1;
            match access {
                Access::Read => {}
                Access::Write => open.writers += 1,
                Access::Append => open.appender = true,
            }
            let handle_state = Handle {
                file,
                access,
                path: op.path.clone(),
                sync_failed: false,
                append_broken: false,
            };
            state.handles.insert(handle, handle_state);
            Ok(handle)
        })
    }

    pub(super) fn close(&mut self, handle: u64) {
        let handle = self.handles.remove(&handle).expect("an open handle exists");
        let file = self.file_mut(handle.file);
        match handle.access {
            Access::Read => {}
            Access::Write => file.writers -= 1,
            Access::Append => file.appender = false,
        }
        self.release(Node::File(handle.file));
    }

    pub(super) fn rename(&mut self, from: &Path, to: &Path) -> io::Result<()> {
        let paths = (canonical(from), Some(canonical(to)));
        self.run(OpKind::Rename, paths, (0, 0), |state, op| {
            let (from_dir, from_name) = state.parent(from, &op.path, ErrorKind::IsADirectory)?;
            let shown = op.to.clone().unwrap_or_default();
            let (to_dir, to_name) = state.parent(to, &shown, ErrorKind::IsADirectory)?;
            let Some(moved) = state.now(from_dir, &from_name) else {
                return Err(refuse(ErrorKind::NotFound, &op.path, "does not exist"));
            };
            let there = state.now(to_dir, &to_name);
            if there == Some(moved) {
                return Ok(());
            }
            let replaced = match (moved, there) {
                (Node::File(_), Some(Node::Dir(_))) => {
                    return Err(refuse(ErrorKind::IsADirectory, &shown, "is a directory"));
                }
                (Node::Dir(dir), _) if state.is_within(to_dir, dir) => {
                    return Err(refuse(
                        ErrorKind::InvalidInput,
                        &shown,
                        "is in the directory that is renamed",
                    ));
                }
                (Node::Dir(_), Some(Node::File(_))) => {
                    return Err(refuse(
                        ErrorKind::NotADirectory,
                        &shown,
                        "is not a directory",
                    ));
                }
                (Node::Dir(_), Some(Node::Dir(there))) => {
                    if state.has_entries(there) {
                        return Err(refuse(ErrorKind::DirectoryNotEmpty, &shown, "is not empty"));
                    }
                    Some(there)
                }
                _ => None,
            };
            state.fail_point(op, true)?;
            state.bind(to_dir, &to_name, Some(moved), op.number);
            state.bind(from_dir, &from_name, None, op.number);
            if let Node::Dir(dir) = moved {
                state.dir_mut(dir).name = Some((to_dir, to_name.clone()));
            }
            let cells = vec![(to_dir, to_name), (from_dir, from_name)];
            state.name_op(op, NameChangeKind::Rename, moved, replaced, cells);
            Ok(())
        })
    }

    fn has_entries(&self, dir: DirId) -> bool {
        self.dirs[&dir]
            .cells
            .values()
            .any(|cell| cell.now.is_some())
    }

    pub(super) fn hard_link(&mut self, original: &Path, link: &Path) -> io::Result<()> {
        let paths = (canonical(original), Some(canonical(link)));
        self.run(OpKind::HardLink, paths, (0, 0), |state, op| {
            let (dir, name) = state.parent(original, &op.path, ErrorKind::PermissionDenied)?;
            let file = match state.now(dir, &name) {
                Some(Node::File(file)) => file,
                Some(Node::Dir(_)) => {
                    return Err(refuse(
                        ErrorKind::PermissionDenied,
                        &op.path,
                        "is a directory, which cannot be linked",
                    ));
                }
                None => return Err(refuse(ErrorKind::NotFound, &op.path, "does not exist")),
            };
            let shown = op.to.clone().unwrap_or_default();
            let (link_dir, link_name) = state.parent(link, &shown, ErrorKind::AlreadyExists)?;
            if state.now(link_dir, &link_name).is_some() {
                return Err(refuse(ErrorKind::AlreadyExists, &shown, "already exists"));
            }
            state.fail_point(op, true)?;
            state.bind(link_dir, &link_name, Some(Node::File(file)), op.number);
            let cells = vec![(link_dir, link_name)];
            state.name_op(op, NameChangeKind::Link, Node::File(file), None, cells);
            Ok(())
        })
    }

    pub(super) fn remove_file(&mut self, path: &Path) -> io::Result<()> {
        let paths = (canonical(path), None);
        self.run(OpKind::RemoveFile, paths, (0, 0), |state, op| {
            let (dir, name) = state.parent(path, &op.path, ErrorKind::IsADirectory)?;
            let file = match state.now(dir, &name) {
                Some(Node::File(file)) => file,
                Some(Node::Dir(_)) => {
                    return Err(refuse(ErrorKind::IsADirectory, &op.path, "is a directory"));
                }
                None => return Err(refuse(ErrorKind::NotFound, &op.path, "does not exist")),
            };
            state.fail_point(op, false)?;
            state.bind(dir, &name, None, op.number);
            let cells = vec![(dir, name)];
            state.name_op(op, NameChangeKind::Remove, Node::File(file), None, cells);
            Ok(())
        })
    }

    pub(super) fn create_dir(&mut self, path: &Path) -> io::Result<()> {
        let paths = (canonical(path), None);
        self.run(OpKind::CreateDir, paths, (0, 0), |state, op| {
            let (dir, name) = state.parent(path, &op.path, ErrorKind::AlreadyExists)?;
            if state.now(dir, &name).is_some() {
                return Err(refuse(ErrorKind::AlreadyExists, &op.path, "already exists"));
            }
            state.fail_point(op, true)?;
            let created = Node::Dir(state.add_dir(dir, &name));
            state.bind(dir, &name, Some(created), op.number);
            let cells = vec![(dir, name)];
            state.name_op(op, NameChangeKind::CreateDir, created, None, cells);
            Ok(())
        })
    }

    pub(super) fn remove_dir(&mut self, path: &Path) -> io::Result<()> {
        let paths = (canonical(path), None);
        self.run(OpKind::RemoveDir, paths, (0, 0), |state, op| {
            let (dir, name) = state.parent(path, &op.path, ErrorKind::ResourceBusy)?;
            let removed = match state.now(dir, &name) {
                Some(Node::Dir(removed)) => removed,
                Some(Node::File(_)) => {
                    return Err(refuse(
                        ErrorKind::NotADirectory,
                        &op.path,
                        "is not a directory",
                    ));
                }
                None => return Err(refuse(ErrorKind::NotFound, &op.path, "does not exist")),
            };
            if state.has_entries(removed) {
                return Err(refuse(
                    ErrorKind::DirectoryNotEmpty,
                    &op.path,
                    "is not empty",
                ));
            }
            state.fail_point(op, false)?;
            state.bind(dir, &name, None, op.number);
            let cells = vec![(dir, name)];
            let kind = NameChangeKind::RemoveDir;
            state.name_op(op, kind, Node::Dir(removed), Some(removed), cells);
            Ok(())
        })
    }

    pub(super) fn read_dir(&mut self, path: &Path) -> io::Result<Vec<DirEntry>> {
        let paths = (canonical(path), None);
        self.run(OpKind::ReadDir, paths, (0, 0), |state, op| {
            let dir = state.dir_at(&components(path)?, &op.path)?;
            state.fail_point(op, false)?;
            let entries = state.dirs[&dir].cells.iter().filter_map(|(name, cell)| {
                let kind = match cell.now? {
                    Node::File(_) => EntryKind::File,
                    Node::Dir(_) => EntryKind::Dir,
                };
                Some(DirEntry {
                    name: name.clone(),
                    kind,
                })
            });
            Ok(entries.collect())
        })
    }

    pub(super) fn sync_dir(&mut self, path: &Path) -> io::Result<()> {
        let paths = (canonical(path), None);
        self.run(OpKind::SyncDir, paths, (0, 0), |state, op| {
            let dir = state.dir_at(&components(path)?, &op.path)?;
            state.fail_point(op, true)?;
            op.changed = state.settle_dir(dir);
            Ok(())
        })
    }

    /// Whether `path` names `file` now. It is no operation: nothing is
    /// counted for it, and no fault falls on it.
    pub(super) fn is_at(&self, path: &Path, file: FileId) -> io::Result<bool> {
        if self.powered_off {
            return Err(SimError::PowerLoss.into());
        }
        // A path the simulator does not take is refused as everywhere,
        // and one that names nothing is answered.
        components(path)?;
        let shown = canonical(path);
        Ok(match self.parent(path, &shown, ErrorKind::IsADirectory) {
            Ok((dir, name)) => self.now(dir, &name) == Some(Node::File(file)),
            Err(_) => false,
        })
    }

    /// Locks the file at `path`, which is made if it is not there and
    /// `create` says so.
    pub(super) fn lock(&mut self, path: &Path, create: bool) -> io::Result<FileId> {
        let paths = (canonical(path), None);
        self.run(OpKind::Lock, paths, (0, 0), |state, op| {
            let (dir, name) = state.parent(path, &op.path, ErrorKind::IsADirectory)?;
            let existing = match state.now(dir, &name) {
                Some(Node::Dir(_)) => {
                    return Err(refuse(ErrorKind::IsADirectory, &op.path, "is a directory"));
                }
                Some(Node::File(file)) if state.locks.contains(&file) => {
                    return Err(refuse(ErrorKind::WouldBlock, &op.path, "is locked"));
                }
                Some(Node::File(file)) => Some(file),
                None if create => None,
                None => return Err(refuse(ErrorKind::NotFound, &op.path, "is not there")),
            };
            let file = state.file_or_create(existing, dir, name, op)?;
            state.locks.insert(file);
            Ok(file)
        })
    }

    pub(super) fn unlock(&mut self, file: FileId) {
        self.locks.remove(&file);
    }

    fn handle(&self, handle: u64) -> (FileId, Access, Paths) {
        let handle = &self.handles[&handle];
        (handle.file, handle.access, (handle.path.clone(), None))
    }

    fn check_len(&self, offset: u64, len: usize, shown: &str) -> io::Result<()> {
        let fits = |end: &u64| *end <= self.config.max_file_len;
        match offset.checked_add(len as u64).filter(fits) {
            Some(_) => Ok(()),
            None => Err(refuse(
                ErrorKind::FileTooLarge,
                shown,
                "would grow past the longest file the simulator holds",
            )),
        }
    }

    /// Makes a change to a file and puts it on record as unsynced.
    fn change(&mut self, file: FileId, kind: FileChangeKind, data: &[u8], op: &mut Op) {
        let sector = self.config.sector_size;
        let open = self.file_mut(file);
        crash::apply(Arc::make_mut(&mut open.content), kind, data, None, sector);
        open.pending.push(Change {
            op: op.number,
            path: op.path.clone(),
            kind,
            data: data.into(),
        });
        op.changed = true;
    }

    fn write(&mut self, file: FileId, offset: u64, data: &[u8], op: &mut Op) {
        if !data.is_empty() {
            let len = data.len() as u64;
            self.change(file, FileChangeKind::Write { offset, len }, data, op);
        }
    }

    pub(super) fn read_at(
        &mut self,
        handle: u64,
        buf: &mut [u8],
        offset: u64,
    ) -> io::Result<usize> {
        let (file, _, paths) = self.handle(handle);
        let extent = (offset, buf.len() as u64);
        self.run(OpKind::Read, paths, extent, |state, op| {
            if buf.is_empty() {
                return Ok(0);
            }
            state.fail_point(op, false)?;
            let content = &state.files[&file].content;
            let from = usize::try_from(offset).ok();
            let Some(available) = from.and_then(|from| content.get(from..)) else {
                return Ok(0);
            };
            let count = available.len().min(buf.len());
            buf[..count].copy_from_slice(&available[..count]);
            Ok(count)
        })
    }

    pub(super) fn write_at(&mut self, handle: u64, buf: &[u8], offset: u64) -> io::Result<()> {
        let (file, access, paths) = self.handle(handle);
        let extent = (offset, buf.len() as u64);
        self.run(OpKind::Write, paths, extent, |state, op| {
            if access != Access::Write {
                return Err(refuse(
                    ErrorKind::InvalidInput,
                    &op.path,
                    "was not opened with OpenMode::Write, which write_at needs",
                ));
            }
            if buf.is_empty() {
                return Ok(());
            }
            state.check_len(offset, buf.len(), &op.path)?;
            let Some(mut injection) = state.fault(op, true) else {
                state.write(file, offset, buf, op);
                return Ok(());
            };
            let sector = state.config.sector_size;
            let mut written = 0;
            for part in crash::failed_write(&mut injection.rng, offset, buf.len() as u64, sector) {
                let within = (part.start - offset) as usize..(part.end - offset) as usize;
                state.write(file, part.start, &buf[within], op);
                written += part.end - part.start;
            }
            Err(op.failure_with(injection.fault, Effect::Wrote(written)))
        })
    }

    pub(super) fn append(&mut self, handle: u64, buf: &[u8]) -> io::Result<u64> {
        let (file, access, paths) = self.handle(handle);
        self.run(OpKind::Append, paths, (0, buf.len() as u64), |state, op| {
            if access != Access::Append {
                return Err(refuse(
                    ErrorKind::InvalidInput,
                    &op.path,
                    "was not opened with OpenMode::Append, which append needs",
                ));
            }
            if state.handles[&handle].append_broken {
                return Err(refuse(
                    ErrorKind::Other,
                    &op.path,
                    "had an append fail that could not be undone, so this handle no longer \
                     knows where the file ends",
                ));
            }
            let offset = state.files[&file].content.len() as u64;
            op.offset = offset;
            if buf.is_empty() {
                return Ok(offset);
            }
            state.check_len(offset, buf.len(), &op.path)?;
            let Some(mut injection) = state.fault(op, true) else {
                state.write(file, offset, buf, op);
                return Ok(offset);
            };
            let sector = state.config.sector_size;
            let written = crash::cut(&mut injection.rng, offset, buf.len() as u64, sector);
            let torn = match injection.torn_append {
                Some(torn) => torn,
                None => injection.rng.chance(state.config.faults.torn_append),
            };
            state.write(file, offset, &buf[..written as usize], op);
            if torn {
                state.handle_mut(handle).append_broken = true;
                return Err(op.failure_with(injection.fault, Effect::AppendTorn(written)));
            }
            if written > 0 {
                state.change(file, FileChangeKind::SetLen { len: offset }, &[], op);
            }
            Err(op.failure_with(injection.fault, Effect::AppendUndone(written)))
        })
    }

    pub(super) fn set_len(&mut self, handle: u64, len: u64) -> io::Result<()> {
        let (file, access, paths) = self.handle(handle);
        self.run(OpKind::SetLen, paths, (0, len), |state, op| {
            if access == Access::Read {
                return Err(refuse(
                    ErrorKind::InvalidInput,
                    &op.path,
                    "was opened with OpenMode::Read, which set_len does not take",
                ));
            }
            state.check_len(len, 0, &op.path)?;
            let current = state.files[&file].content.len() as u64;
            state.fail_point(op, len > current)?;
            if len != current {
                state.change(file, FileChangeKind::SetLen { len }, &[], op);
            }
            Ok(())
        })
    }

    pub(super) fn len(&mut self, handle: u64) -> io::Result<u64> {
        let (file, _, paths) = self.handle(handle);
        self.run(OpKind::Len, paths, (0, 0), |state, op| {
            state.fail_point(op, false)?;
            Ok(state.files[&file].content.len() as u64)
        })
    }

    pub(super) fn sync(&mut self, handle: u64, kind: OpKind) -> io::Result<()> {
        let (file, _, paths) = self.handle(handle);
        self.run(kind, paths, (0, 0), |state, op| {
            if state.handles[&handle].sync_failed {
                return Err(refuse(
                    ErrorKind::Other,
                    &op.path,
                    "had a sync fail on this handle, which can no longer make it durable",
                ));
            }
            if let Some(injection) = state.fault(op, true) {
                state.handle_mut(handle).sync_failed = true;
                let open = state.file_mut(file);
                open.unknown = open.pending.len();
                let unknown = open.unknown as u64;
                return Err(op.failure_with(injection.fault, Effect::SyncFailed(unknown)));
            }
            state.settle_file(file, op);
            Ok(())
        })
    }

    /// Makes the file durable. The changes made since the last failed sync
    /// are covered. What became of the ones before it is settled now, as the
    /// configuration says: nothing later can tell when the system dropped or
    /// wrote them.
    fn settle_file(&mut self, file: FileId, op: &mut Op) {
        let sector = self.config.sector_size;
        let open = self.files.get_mut(&file).expect("a file in use exists");
        if open.pending.is_empty() {
            return;
        }
        let durable = Arc::make_mut(&mut open.durable);
        let (unknown, covered) = open.pending.split_at(open.unknown);
        if !unknown.is_empty() {
            let kinds = unknown.iter().map(|change| change.kind);
            let survivors = match self.config.unknown {
                UnknownFate::Lost => Vec::new(),
                UnknownFate::Kept => (0..unknown.len()).map(Survivor::whole).collect(),
                UnknownFate::Drawn(draw) => {
                    let mut at_sync = SimRng::derive(self.config.seed, stream::SETTLE, op.number);
                    let mut rng = SimRng::derive(at_sync.next_u64(), stream::DRAW, draw);
                    crash::sample_changes(&mut rng, kinds, &self.config.crash, sector)
                }
            };
            for survivor in &survivors {
                let change = &unknown[survivor.change];
                let tear = survivor.tear.as_ref();
                crash::apply(durable, change.kind, &change.data, tear, sector);
            }
            let described = unknown
                .iter()
                .map(|change| (change.op, &*change.path, change.kind));
            self.trace.push(Event::Settled {
                op: op.number,
                path: op.path.to_string(),
                changes: crash::fates(described, &survivors),
            });
        }
        for change in covered {
            crash::apply(durable, change.kind, &change.data, None, sector);
        }
        open.pending.clear();
        open.unknown = 0;
        op.changed = true;
    }

    pub(super) fn image(&self) -> CrashImage {
        let cells = self.dirs.values().flat_map(|dir| dir.cells.values());
        let named: BTreeSet<FileId> = cells
            .flat_map(Cell::nodes)
            .filter_map(|node| match node {
                Node::File(file) => Some(file),
                Node::Dir(_) => None,
            })
            .collect();
        let mut files = BTreeMap::new();
        let mut changes = Vec::new();
        for id in named {
            let file = &self.files[&id];
            files.insert(id, file.durable.clone());
            for (index, change) in file.pending.iter().enumerate() {
                changes.push(FileChange {
                    op: change.op,
                    path: change.path.to_string(),
                    file: id.0,
                    kind: change.kind,
                    sectors: crash::sectors_of(change.kind, self.config.sector_size),
                    unknown: index < file.unknown,
                    data: change.data.clone(),
                });
            }
        }
        changes.sort_by_key(|change| change.op);
        let names = self.name_ops.iter().map(|(op, name)| NameChange {
            op: *op,
            kind: name.kind,
            path: name.path.to_string(),
            to: name.to.as_deref().map(str::to_owned),
            file: name.file.map(|file| file.0),
        });
        CrashImage {
            config: self.config.clone(),
            ops: self.stats.ops,
            names: names.collect(),
            dirs: self.dirs.clone(),
            name_ops: self.name_ops.clone(),
            files,
            changes,
        }
    }

    pub(super) fn fingerprint(&self) -> u64 {
        let mut hasher = Fnv::new();
        self.hash_dir(ROOT, &mut hasher, &mut BTreeMap::new());
        hasher.0
    }

    fn hash_dir(&self, dir: DirId, hasher: &mut Fnv, seen: &mut BTreeMap<FileId, u64>) {
        for (name, cell) in &self.dirs[&dir].cells {
            let Some(node) = cell.now else {
                continue;
            };
            hasher.u64(name.len() as u64);
            hasher.bytes(name.as_encoded_bytes());
            match node {
                Node::Dir(dir) => {
                    hasher.u64(1);
                    self.hash_dir(dir, hasher, seen);
                    hasher.u64(2);
                }
                Node::File(file) => {
                    let label = seen.len() as u64;
                    if let Some(known) = seen.get(&file) {
                        hasher.u64(3);
                        hasher.u64(*known);
                        continue;
                    }
                    seen.insert(file, label);
                    let content = &self.files[&file].content;
                    hasher.u64(4);
                    hasher.u64(content.len() as u64);
                    hasher.bytes(content);
                }
            }
        }
    }

    /// The buffers of the file at `path`: what operations see, and what is
    /// durable.
    #[cfg(test)]
    pub(super) fn buffers(&self, path: &Path) -> (Arc<Vec<u8>>, Arc<Vec<u8>>) {
        let (dir, name) = self.parent(path, "", ErrorKind::IsADirectory).unwrap();
        let Some(Node::File(file)) = self.now(dir, &name) else {
            panic!("{} is not a file", path.display());
        };
        let file = &self.files[&file];
        (file.content.clone(), file.durable.clone())
    }

    /// Checks what the simulator relies on about its own state.
    #[cfg(test)]
    pub(super) fn check(&self) {
        let mut file_refs: BTreeMap<FileId, u32> = BTreeMap::new();
        let mut dir_refs: BTreeMap<DirId, u32> = BTreeMap::from([(ROOT, 1)]);
        let mut named = BTreeSet::new();
        for (id, dir) in &self.dirs {
            for (name, cell) in &dir.cells {
                if let Some(Node::Dir(shown)) = cell.now {
                    assert!(named.insert(shown), "a directory has one name");
                    let place = Some((*id, name.clone()));
                    assert_eq!(self.dirs[&shown].name, place, "and knows it");
                }
                assert!(
                    cell.now.is_some() || cell.durable.is_some() || !cell.pending.is_empty(),
                    "an empty cell is kept"
                );
                for node in cell.nodes() {
                    match node {
                        Node::File(file) => *file_refs.entry(file).or_default() += 1,
                        Node::Dir(dir) => *dir_refs.entry(dir).or_default() += 1,
                    }
                }
                let expected = cell.pending.last().map_or(cell.durable, |last| last.1);
                assert_eq!(cell.now, expected, "a name shows its newest change");
                assert!(cell.pending.is_sorted_by_key(|(seq, _)| *seq));
                for (seq, _) in &cell.pending {
                    let cells = &self.name_ops[seq].cells;
                    assert!(cells.contains(&(*id, name.clone())));
                }
            }
        }
        for (seq, op) in &self.name_ops {
            assert!(
                !op.waiting.is_empty(),
                "a synced change of names is durable"
            );
            assert!(op.waiting.iter().all(|dir| self.dirs.contains_key(dir)));
            for (dir, name) in &op.cells {
                let pending = &self.dirs[dir].cells[name].pending;
                assert!(pending.iter().any(|(pending, _)| pending == seq));
            }
            let kept = with_what_they_build_on(&self.dirs, &self.name_ops, [*seq]);
            assert!(kept.iter().all(|built_on| built_on <= seq));
        }
        let mut under_the_root = BTreeSet::new();
        let mut walk = vec![ROOT];
        while let Some(dir) = walk.pop() {
            for cell in self.dirs[&dir].cells.values() {
                if let Some(Node::Dir(below)) = cell.now {
                    assert!(under_the_root.insert(below), "the names are a tree");
                    walk.push(below);
                }
            }
        }
        assert_eq!(under_the_root, named, "a directory with a name is found");
        for handle in self.handles.values() {
            *file_refs.entry(handle.file).or_default() += 1;
        }
        for (id, file) in &self.files {
            assert_eq!(Some(&file.refs), file_refs.get(id), "references of {id:?}");
            assert!(file.unknown <= file.pending.len());
            let handles = || self.handles.values().filter(|handle| handle.file == *id);
            let writers = handles().filter(|handle| handle.access == Access::Write);
            let appenders = handles().filter(|handle| handle.access == Access::Append);
            assert_eq!(file.writers as usize, writers.count());
            assert_eq!(usize::from(file.appender), appenders.count());
            assert!(!file.appender || file.writers == 0);
        }
        for (id, dir) in &self.dirs {
            assert_eq!(Some(&dir.refs), dir_refs.get(id), "references of {id:?}");
        }
        assert_eq!(file_refs.len(), self.files.len());
        assert_eq!(dir_refs.len(), self.dirs.len());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `/d` and `/d/e`, and then `/d` under the name `d` in `/d/e` as well:
    /// each of the two is above the other, as no operation leaves them.
    fn with_a_cycle() -> (State, DirId) {
        let mut state = State::new(SimConfig::new(1));
        state.create_dir(Path::new("/d")).unwrap();
        state.create_dir(Path::new("/d/e")).unwrap();
        let (d_name, e_name) = (OsStr::new("d"), OsStr::new("e"));
        let d = state.dir_at(&[d_name], "/d").unwrap();
        let e = state.dir_at(&[d_name, e_name], "/d/e").unwrap();

        assert!(state.is_within(e, d) && state.is_within(e, ROOT));
        assert!(!state.is_within(d, e));
        let mut placed = Vec::new();
        state.placed(e, &mut placed);
        assert_eq!(placed.len(), 2);

        let d_in_e = Cell {
            now: Some(Node::Dir(d)),
            ..Cell::default()
        };
        state.dir_mut(e).cells.insert(d_name.to_owned(), d_in_e);
        state.dir_mut(d).name = Some((e, d_name.to_owned()));
        (state, d)
    }

    #[test]
    #[should_panic(
        expected = "the simulator's directories form a cycle: the directory named \"e\" is above itself"
    )]
    fn what_placed_a_directory_is_not_asked_for_ever_of_a_cycle() {
        let (state, d) = with_a_cycle();
        state.placed(d, &mut Vec::new());
    }

    #[test]
    #[should_panic(
        expected = "the simulator's directories form a cycle: the directory named \"e\" is above itself"
    )]
    fn what_a_directory_is_within_is_not_asked_for_ever_of_a_cycle() {
        let (state, d) = with_a_cycle();
        state.is_within(d, ROOT);
    }
}
