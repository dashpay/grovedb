use std::{
    io::{self, ErrorKind},
    path::Path,
    sync::{Arc, Mutex},
};

use lignin_io::{DirEntry, OpenMode, Vfs, VfsFile, VfsLock};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Op {
    Open,
    Rename,
    HardLink,
    RemoveFile,
    CreateDir,
    RemoveDir,
    ReadDir,
    SyncDir,
    Lock,
    Read,
    Write,
    Append,
    SyncData,
    SyncAll,
    Len,
    SetLen,
}

#[derive(Debug)]
struct Fault {
    op: Op,
    name: String,
    skip: usize,
}

#[derive(Debug, Default)]
struct Script {
    calls: Vec<(Op, String)>,
    faults: Vec<Fault>,
    skip_syncs: bool,
}

/// Passes every call on to another file layer, records it, and fails the
/// calls it was told to fail without making them.
#[derive(Debug, Clone)]
pub(crate) struct FaultyVfs {
    inner: Arc<dyn Vfs>,
    script: Arc<Mutex<Script>>,
}

pub(crate) const INJECTED: ErrorKind = ErrorKind::PermissionDenied;

fn name_of(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Records the call and says whether to pass it on: `Ok(false)` for a sync
/// that is skipped.
fn enter(script: &Mutex<Script>, op: Op, name: &str) -> io::Result<bool> {
    let mut script = script.lock().unwrap();
    script.calls.push((op, name.to_owned()));
    let is_sync = matches!(op, Op::SyncData | Op::SyncAll | Op::SyncDir);
    let pass_on = !(is_sync && script.skip_syncs);
    let Some(at) = script
        .faults
        .iter()
        .position(|fault| fault.op == op && fault.name == name)
    else {
        return Ok(pass_on);
    };
    if script.faults[at].skip > 0 {
        script.faults[at].skip -= 1;
        return Ok(pass_on);
    }
    script.faults.remove(at);
    Err(io::Error::new(INJECTED, format!("injected into {op:?}")))
}

impl FaultyVfs {
    pub(crate) fn new(inner: Arc<dyn Vfs>) -> Self {
        Self {
            inner,
            script: Arc::default(),
        }
    }

    /// As [`Self::new`], and no sync reaches the file layer below. A test
    /// in which nothing crashes loses nothing by that, and does not wait for
    /// a disk.
    pub(crate) fn without_syncs(inner: Arc<dyn Vfs>) -> Self {
        let vfs = Self::new(inner);
        vfs.script.lock().unwrap().skip_syncs = true;
        vfs
    }

    /// Fails the next `op` on the file or directory called `name`.
    pub(crate) fn fail(&self, op: Op, name: &str) {
        self.fail_after(0, op, name);
    }

    /// Fails the `op` on `name` that follows `skip` more of them.
    pub(crate) fn fail_after(&self, skip: usize, op: Op, name: &str) {
        self.script.lock().unwrap().faults.push(Fault {
            op,
            name: name.to_owned(),
            skip,
        });
    }

    /// The calls made since the last time this was asked.
    pub(crate) fn take_calls(&self) -> Vec<(Op, String)> {
        std::mem::take(&mut self.script.lock().unwrap().calls)
    }

    pub(crate) fn pending_faults(&self) -> usize {
        self.script.lock().unwrap().faults.len()
    }

    fn enter(&self, op: Op, path: &Path) -> io::Result<bool> {
        enter(&self.script, op, &name_of(path))
    }
}

impl Vfs for FaultyVfs {
    fn open(&self, path: &Path, mode: OpenMode) -> io::Result<Box<dyn VfsFile>> {
        self.enter(Op::Open, path)?;
        Ok(Box::new(FaultyFile {
            inner: self.inner.open(path, mode)?,
            name: name_of(path),
            script: self.script.clone(),
        }))
    }

    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        self.enter(Op::Rename, from)?;
        self.inner.rename(from, to)
    }

    fn hard_link(&self, original: &Path, link: &Path) -> io::Result<()> {
        self.enter(Op::HardLink, original)?;
        self.inner.hard_link(original, link)
    }

    fn remove_file(&self, path: &Path) -> io::Result<()> {
        self.enter(Op::RemoveFile, path)?;
        self.inner.remove_file(path)
    }

    fn create_dir(&self, path: &Path) -> io::Result<()> {
        self.enter(Op::CreateDir, path)?;
        self.inner.create_dir(path)
    }

    fn remove_dir(&self, path: &Path) -> io::Result<()> {
        self.enter(Op::RemoveDir, path)?;
        self.inner.remove_dir(path)
    }

    fn read_dir(&self, path: &Path) -> io::Result<Vec<DirEntry>> {
        self.enter(Op::ReadDir, path)?;
        self.inner.read_dir(path)
    }

    fn sync_dir(&self, path: &Path) -> io::Result<()> {
        if self.enter(Op::SyncDir, path)? {
            self.inner.sync_dir(path)?;
        }
        Ok(())
    }

    fn lock(&self, path: &Path) -> io::Result<Box<dyn VfsLock>> {
        self.enter(Op::Lock, path)?;
        self.inner.lock(path)
    }
}

#[derive(Debug)]
struct FaultyFile {
    inner: Box<dyn VfsFile>,
    name: String,
    script: Arc<Mutex<Script>>,
}

impl FaultyFile {
    fn enter(&self, op: Op) -> io::Result<bool> {
        enter(&self.script, op, &self.name)
    }
}

impl VfsFile for FaultyFile {
    fn read_at(&self, buf: &mut [u8], offset: u64) -> io::Result<usize> {
        self.enter(Op::Read)?;
        self.inner.read_at(buf, offset)
    }

    fn write_at(&self, buf: &[u8], offset: u64) -> io::Result<()> {
        self.enter(Op::Write)?;
        self.inner.write_at(buf, offset)
    }

    fn append(&self, buf: &[u8]) -> io::Result<u64> {
        self.enter(Op::Append)?;
        self.inner.append(buf)
    }

    fn sync_data(&self) -> io::Result<()> {
        if self.enter(Op::SyncData)? {
            self.inner.sync_data()?;
        }
        Ok(())
    }

    fn sync_all(&self) -> io::Result<()> {
        if self.enter(Op::SyncAll)? {
            self.inner.sync_all()?;
        }
        Ok(())
    }

    fn len(&self) -> io::Result<u64> {
        self.enter(Op::Len)?;
        self.inner.len()
    }

    fn set_len(&self, len: u64) -> io::Result<()> {
        self.enter(Op::SetLen)?;
        self.inner.set_len(len)
    }
}
