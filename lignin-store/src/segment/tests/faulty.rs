use std::{
    io::{self, ErrorKind},
    path::Path,
    sync::{
        mpsc::{self, Receiver, Sender},
        Arc, Mutex,
    },
    time::Duration,
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

/// The `op` on `name` that follows `skip` more of them.
#[derive(Debug)]
struct Call {
    op: Op,
    name: String,
    skip: usize,
}

/// The place in `awaited` of the call that is being made, if it is one of
/// them. One that has others to skip first has one fewer afterwards.
fn due<'a>(awaited: impl Iterator<Item = &'a mut Call>, op: Op, name: &str) -> Option<usize> {
    let (at, call) = awaited
        .enumerate()
        .find(|(_, call)| call.op == op && call.name == name)?;
    if call.skip > 0 {
        call.skip -= 1;
        return None;
    }
    Some(at)
}

#[derive(Debug)]
struct Stop {
    at: Call,
    reached: Sender<()>,
    go_on: Receiver<()>,
}

impl Stop {
    fn wait(self) {
        let _ = self.reached.send(());
        let _ = self.go_on.recv();
    }
}

/// A call that [`FaultyVfs::stop_after`] stops, as the test sees it. The
/// call goes on when this is dropped, so a test that fails leaves no thread
/// waiting.
#[derive(Debug)]
pub(crate) struct Stopped {
    reached: Receiver<()>,
    go_on: Sender<()>,
}

impl Stopped {
    const PATIENCE: Duration = Duration::from_secs(60);

    /// Waits until the call is made. It then waits, before it reaches the
    /// file layer below, until this is dropped.
    #[track_caller]
    pub(crate) fn wait(&self) {
        assert!(
            self.reached.recv_timeout(Self::PATIENCE).is_ok(),
            "the call that was to stop was never made"
        );
    }

    pub(crate) fn go_on(self) {}
}

impl Drop for Stopped {
    fn drop(&mut self) {
        let _ = self.go_on.send(());
    }
}

#[derive(Debug, Default)]
struct Script {
    calls: Vec<(Op, String)>,
    faults: Vec<Call>,
    stops: Vec<Stop>,
    skip_syncs: bool,
}

impl Script {
    fn stop_at(&mut self, op: Op, name: &str) -> Option<Stop> {
        let at = due(self.stops.iter_mut().map(|stop| &mut stop.at), op, name)?;
        Some(self.stops.remove(at))
    }
}

/// Passes every call on to another file layer, records it, and fails the
/// calls it was told to fail without making them. A call it was told to stop
/// waits before it is recorded and passed on.
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
    let stop = script.lock().unwrap().stop_at(op, name);
    if let Some(stop) = stop {
        stop.wait();
    }
    let mut script = script.lock().unwrap();
    script.calls.push((op, name.to_owned()));
    let is_sync = matches!(op, Op::SyncData | Op::SyncAll | Op::SyncDir);
    let pass_on = !(is_sync && script.skip_syncs);
    let Some(at) = due(script.faults.iter_mut(), op, name) else {
        return Ok(pass_on);
    };
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
        self.script.lock().unwrap().faults.push(Call {
            op,
            name: name.to_owned(),
            skip,
        });
    }

    /// Stops the `op` on `name` that follows `skip` more of them, in the
    /// thread that makes it, until the test lets it go on.
    pub(crate) fn stop_after(&self, skip: usize, op: Op, name: &str) -> Stopped {
        let (reached, was_reached) = mpsc::channel();
        let (go_on, told_to_go_on) = mpsc::channel();
        self.script.lock().unwrap().stops.push(Stop {
            at: Call {
                op,
                name: name.to_owned(),
                skip,
            },
            reached,
            go_on: told_to_go_on,
        });
        Stopped {
            reached: was_reached,
            go_on,
        }
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

    fn lock_existing(&self, path: &Path) -> io::Result<Box<dyn VfsLock>> {
        self.enter(Op::Lock, path)?;
        self.inner.lock_existing(path)
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
