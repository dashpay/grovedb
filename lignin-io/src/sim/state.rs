//! The simulated file system: one value behind one lock, in which every
//! operation runs from start to end.

use std::{
    collections::{btree_map::Entry, BTreeMap, BTreeSet},
    ffi::{OsStr, OsString},
    io::{self, ErrorKind},
    path::{Component, Path},
    sync::Arc,
};

use super::{
    crash::{self, Change, CrashImage, FileChange, FileChangeKind, NameChange, NameChangeKind},
    fault::{Fault, FaultRule, OpKind, OpSet, SimError},
    rng::SimRng,
    trace::{Effect, Event, Fnv, OpRecord, Outcome},
    CrashPoint, SimConfig,
};
use crate::vfs::{Create, DirEntry, EntryKind, OpenMode};

const STREAM_FAULT: u64 = 1;
const STREAM_DETAIL: u64 = 2;
const STREAM_SETTLE: u64 = 3;

pub(super) const ROOT: DirId = DirId(0);

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
/// `durable`, or the newest of the `pending` changes that it keeps and that
/// is newer than `durable_seq`. A change is known by the number of the
/// operation that made it.
#[derive(Clone, Debug, Default)]
pub(super) struct Cell {
    now: Option<Node>,
    pub(super) durable: Option<Node>,
    pub(super) durable_seq: u64,
    pub(super) pending: Vec<(u64, Option<Node>)>,
}

impl Cell {
    fn nodes(&self) -> impl Iterator<Item = Node> + '_ {
        let pending = self.pending.iter().filter_map(|(_, node)| *node);
        self.now.into_iter().chain(self.durable).chain(pending)
    }
}

#[derive(Debug)]
struct Dir {
    cells: BTreeMap<OsString, Cell>,
    refs: u32,
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
/// directory in `waiting` was synced.
#[derive(Debug)]
struct NameOp {
    kind: NameChangeKind,
    path: Arc<str>,
    to: Option<Arc<str>>,
    waiting: Vec<DirId>,
    cells: Vec<(DirId, OsString)>,
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
    pub(super) ops: u64,
    pub(super) syncs: u64,
    pub(super) changes: u64,
    pub(super) faults: u64,
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
        let root = Dir {
            cells: BTreeMap::new(),
            refs: 1,
        };
        Self {
            config,
            rules: Vec::new(),
            ops: 0,
            syncs: 0,
            changes: 0,
            faults: 0,
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

    pub(super) fn add_dir(&mut self) -> DirId {
        let id = DirId(self.id());
        let dir = Dir {
            cells: BTreeMap::new(),
            refs: 0,
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
            durable_seq: 0,
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
                if dir.refs > 0 {
                    return;
                }
                let dir = self.dirs.remove(&id).expect("the directory was just used");
                for (name, cell) in dir.cells {
                    for (seq, _) in &cell.pending {
                        self.forget_cell(*seq, id, &name);
                    }
                    for node in cell.nodes() {
                        self.release(node);
                    }
                }
            }
        }
    }

    /// A directory that nothing can bring back takes its names with it. A
    /// change that also has a name elsewhere stays, and can no longer be
    /// settled.
    fn forget_cell(&mut self, seq: u64, dir: DirId, name: &OsStr) {
        let Entry::Occupied(mut op) = self.name_ops.entry(seq) else {
            unreachable!("a pending change of names is on record");
        };
        op.get_mut()
            .cells
            .retain(|(cell_dir, cell_name)| *cell_dir != dir || cell_name != name);
        if op.get().cells.is_empty() {
            op.remove();
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

    fn name_op(&mut self, op: &mut Op, kind: NameChangeKind, cells: Vec<(DirId, OsString)>) {
        let mut waiting: Vec<DirId> = cells.iter().map(|(dir, _)| *dir).collect();
        waiting.dedup();
        let name_op = NameOp {
            kind,
            path: op.path.clone(),
            to: op.to.clone(),
            waiting,
            cells,
        };
        self.name_ops.insert(op.number, name_op);
        op.changed = true;
    }

    fn create_file(&mut self, dir: DirId, name: &OsStr, op: &mut Op) -> FileId {
        let file = self.add_file(Arc::default());
        self.bind(dir, name, Some(Node::File(file)), op.number);
        self.name_op(op, NameChangeKind::Create, vec![(dir, name.to_owned())]);
        file
    }

    fn settle_dir(&mut self, dir: DirId) -> bool {
        let cells = self.dirs[&dir].cells.values();
        let seqs: BTreeSet<u64> = cells
            .flat_map(|cell| cell.pending.iter().map(|(seq, _)| *seq))
            .collect();
        let mut changed = false;
        for seq in seqs {
            let Entry::Occupied(mut op) = self.name_ops.entry(seq) else {
                unreachable!("a pending change of names is on record");
            };
            let waiting = &mut op.get_mut().waiting;
            let before = waiting.len();
            waiting.retain(|waited| *waited != dir);
            changed |= waiting.len() < before;
            if !waiting.is_empty() {
                continue;
            }
            for (cell_dir, name) in op.remove().cells {
                self.settle_cell(cell_dir, &name, seq);
            }
        }
        changed
    }

    fn settle_cell(&mut self, dir: DirId, name: &OsStr, seq: u64) {
        let cells = &mut self.dir_mut(dir).cells;
        let Entry::Occupied(mut entry) = cells.entry(name.to_owned()) else {
            unreachable!("a pending change of names has its name");
        };
        let cell = entry.get_mut();
        let at = cell.pending.iter().position(|(pending, _)| *pending == seq);
        let (_, node) = cell
            .pending
            .remove(at.expect("the change is pending on its name"));
        let dropped = if seq > cell.durable_seq {
            cell.durable_seq = seq;
            std::mem::replace(&mut cell.durable, node)
        } else {
            node
        };
        if cell.now.is_none() && cell.durable.is_none() && cell.pending.is_empty() {
            entry.remove();
        }
        if let Some(node) = dropped {
            self.release(node);
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
        self.ops += 1;
        if OpSet::SYNCS.contains(kind) {
            self.syncs += 1;
        }
        let mut op = Op {
            number: self.ops,
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
            self.changes += 1;
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
            Some(CrashPoint::AfterOp(count)) => self.ops >= count,
            Some(CrashPoint::AfterSync(count)) => self.syncs >= count,
            Some(CrashPoint::AfterChange(count)) => self.changes >= count,
        };
        if reached {
            self.power_off();
        }
    }

    pub(super) fn power_off(&mut self) {
        if !self.powered_off {
            self.powered_off = true;
            self.crash_point = None;
            self.trace.push(Event::PowerOff { after_op: self.ops });
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
            let mut rng = SimRng::derive(self.config.seed, STREAM_FAULT, op.number);
            let can_be_full = needs_space && odds.enospc_ops.contains(op.kind);
            if odds.eio_ops.contains(op.kind) && rng.chance(odds.eio) {
                hit = Some((Fault::Eio, None));
            } else if can_be_full && rng.chance(odds.enospc) {
                hit = Some((Fault::Enospc, None));
            }
        }
        let (fault, torn_append) = hit?;
        self.faults += 1;
        op.injected = Some((fault, Effect::Nothing));
        Some(Injection {
            fault,
            rng: SimRng::derive(self.config.seed, STREAM_DETAIL, op.number),
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
            state.fail_point(op, existing.is_none())?;
            let file = match existing {
                Some(file) => file,
                None => state.create_file(dir, &name, op),
            };
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
            let file = match state.now(from_dir, &from_name) {
                Some(Node::File(file)) => file,
                Some(Node::Dir(_)) => {
                    return Err(refuse(ErrorKind::IsADirectory, &op.path, "is a directory"));
                }
                None => return Err(refuse(ErrorKind::NotFound, &op.path, "does not exist")),
            };
            match state.now(to_dir, &to_name) {
                Some(Node::Dir(_)) => {
                    return Err(refuse(ErrorKind::IsADirectory, &shown, "is a directory"));
                }
                Some(Node::File(other)) if other == file => return Ok(()),
                _ => {}
            }
            state.fail_point(op, true)?;
            state.bind(to_dir, &to_name, Some(Node::File(file)), op.number);
            state.bind(from_dir, &from_name, None, op.number);
            let cells = vec![(to_dir, to_name), (from_dir, from_name)];
            state.name_op(op, NameChangeKind::Rename, cells);
            Ok(())
        })
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
            state.name_op(op, NameChangeKind::Link, vec![(link_dir, link_name)]);
            Ok(())
        })
    }

    pub(super) fn remove_file(&mut self, path: &Path) -> io::Result<()> {
        let paths = (canonical(path), None);
        self.run(OpKind::RemoveFile, paths, (0, 0), |state, op| {
            let (dir, name) = state.parent(path, &op.path, ErrorKind::IsADirectory)?;
            match state.now(dir, &name) {
                Some(Node::File(_)) => {}
                Some(Node::Dir(_)) => {
                    return Err(refuse(ErrorKind::IsADirectory, &op.path, "is a directory"));
                }
                None => return Err(refuse(ErrorKind::NotFound, &op.path, "does not exist")),
            }
            state.fail_point(op, false)?;
            state.bind(dir, &name, None, op.number);
            state.name_op(op, NameChangeKind::Remove, vec![(dir, name)]);
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
            let created = state.add_dir();
            state.bind(dir, &name, Some(Node::Dir(created)), op.number);
            state.name_op(op, NameChangeKind::CreateDir, vec![(dir, name)]);
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
            let mut cells = state.dirs[&removed].cells.values();
            if cells.any(|cell| cell.now.is_some()) {
                return Err(refuse(
                    ErrorKind::DirectoryNotEmpty,
                    &op.path,
                    "is not empty",
                ));
            }
            state.fail_point(op, false)?;
            state.bind(dir, &name, None, op.number);
            state.name_op(op, NameChangeKind::RemoveDir, vec![(dir, name)]);
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

    pub(super) fn lock(&mut self, path: &Path) -> io::Result<FileId> {
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
                None => None,
            };
            state.fail_point(op, existing.is_none())?;
            let file = match existing {
                Some(file) => file,
                None => state.create_file(dir, &name, op),
            };
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
        let fits = |end: &u64| *end <= self.config.max_file_len && usize::try_from(*end).is_ok();
        match offset.checked_add(len as u64).filter(fits) {
            Some(_) => Ok(()),
            None => Err(refuse(
                ErrorKind::FileTooLarge,
                shown,
                "would grow past the longest file the simulator holds",
            )),
        }
    }

    fn write(&mut self, file: FileId, offset: u64, data: &[u8], op: &mut Op) {
        if data.is_empty() {
            return;
        }
        let (start, end) = (offset as usize, offset as usize + data.len());
        let open = self.file_mut(file);
        let content = Arc::make_mut(&mut open.content);
        if content.len() < end {
            content.resize(end, 0);
        }
        content[start..end].copy_from_slice(data);
        open.pending.push(Change {
            op: op.number,
            path: op.path.clone(),
            kind: FileChangeKind::Write {
                offset,
                len: data.len() as u64,
            },
            data: data.into(),
        });
        op.changed = true;
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
            state.check_len(offset, buf.len(), &op.path)?;
            if let Some(mut injection) = state.fault(op, true) {
                let sector = state.config.sector_size;
                let written = crash::cut(&mut injection.rng, offset, buf.len() as u64, sector);
                state.write(file, offset, &buf[..written as usize], op);
                return Err(op.failure_with(injection.fault, Effect::Wrote(written)));
            }
            state.write(file, offset, buf, op);
            Ok(())
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
                state.resize(file, offset, op);
            }
            Err(op.failure_with(injection.fault, Effect::AppendUndone(written)))
        })
    }

    fn resize(&mut self, file: FileId, len: u64, op: &mut Op) {
        let open = self.file_mut(file);
        Arc::make_mut(&mut open.content).resize(len as usize, 0);
        open.pending.push(Change {
            op: op.number,
            path: op.path.clone(),
            kind: FileChangeKind::SetLen { len },
            data: Arc::default(),
        });
        op.changed = true;
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
                state.resize(file, len, op);
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
    /// are covered. What became of the ones before it is drawn from the seed
    /// now: nothing later can tell when the system dropped or wrote them.
    fn settle_file(&mut self, file: FileId, op: &mut Op) {
        let sector = self.config.sector_size;
        let open = self.files.get_mut(&file).expect("a file in use exists");
        if open.pending.is_empty() {
            return;
        }
        let durable = Arc::make_mut(&mut open.durable);
        let (unknown, covered) = open.pending.split_at(open.unknown);
        if !unknown.is_empty() {
            let mut rng = SimRng::derive(self.config.seed, STREAM_SETTLE, op.number);
            let kinds = unknown.iter().map(|change| change.kind);
            let survivors = crash::sample_changes(&mut rng, kinds, &self.config.crash, sector);
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
        let mut named = BTreeSet::new();
        let mut dirs = BTreeMap::new();
        for (id, dir) in &self.dirs {
            for node in dir.cells.values().flat_map(Cell::nodes) {
                if let Node::File(file) = node {
                    named.insert(file);
                }
            }
            dirs.insert(*id, dir.cells.clone());
        }
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
        });
        CrashImage {
            config: self.config.clone(),
            ops: self.ops,
            dirs,
            files,
            changes,
            names: names.collect(),
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
            hasher.text(&name.to_string_lossy());
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
        for (id, dir) in &self.dirs {
            for (name, cell) in &dir.cells {
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
                let newest = cell.pending.iter().map(|(seq, _)| *seq).max();
                let expected = match newest {
                    Some(seq) if seq > cell.durable_seq => {
                        let last = cell.pending.iter().rfind(|(pending, _)| *pending == seq);
                        last.unwrap().1
                    }
                    _ => cell.durable,
                };
                assert_eq!(cell.now, expected, "a name shows its newest change");
                for (seq, _) in &cell.pending {
                    let cells = &self.name_ops[seq].cells;
                    assert!(cells.contains(&(*id, name.clone())));
                }
            }
        }
        for (seq, op) in &self.name_ops {
            assert!(!op.cells.is_empty() && !op.waiting.is_empty());
            for (dir, name) in &op.cells {
                let pending = &self.dirs[dir].cells[name].pending;
                assert!(pending.iter().any(|(pending, _)| pending == seq));
            }
        }
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
