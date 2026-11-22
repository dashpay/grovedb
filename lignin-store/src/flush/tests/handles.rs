//! A file layer that knows its handles apart: which one appended to a
//! file of the value log, and which one synced it.

use std::{
    io,
    path::Path,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
};

use lignin_io::{DirEntry, OpenMode, Vfs, VfsFile, VfsLock};

/// What one handle did to a file of the value log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Seen {
    /// The handle's number: each call that opens a file gives out a new
    /// one.
    pub(crate) handle: u64,
    /// `"append"` or `"sync"`.
    pub(crate) op: &'static str,
    /// The file's name.
    pub(crate) file: String,
    /// Whether the file layer below answered without an error.
    pub(crate) ok: bool,
}

/// The file layer `inner`, with every handle numbered and the appends and
/// syncs of value-log files noted.
#[derive(Debug)]
pub(crate) struct Handles {
    inner: Arc<dyn Vfs>,
    next: AtomicU64,
    seen: Arc<Mutex<Vec<Seen>>>,
}

impl Handles {
    pub(crate) fn over(inner: Arc<dyn Vfs>) -> Arc<Self> {
        Arc::new(Self {
            inner,
            next: AtomicU64::new(1),
            seen: Arc::default(),
        })
    }

    /// What the handles did to the value-log file `file` so far, in
    /// order.
    pub(crate) fn seen(&self, file: &str) -> Vec<Seen> {
        let seen = self.seen.lock().expect("what was seen");
        seen.iter()
            .filter(|seen| seen.file == file)
            .cloned()
            .collect()
    }
}

#[derive(Debug)]
struct Numbered {
    inner: Box<dyn VfsFile>,
    handle: u64,
    /// The name of the file, if it is one of the value log.
    file: Option<String>,
    seen: Arc<Mutex<Vec<Seen>>>,
}

impl Numbered {
    fn note<T>(&self, op: &'static str, answered: &io::Result<T>) {
        if let Some(file) = &self.file {
            self.seen.lock().expect("what was seen").push(Seen {
                handle: self.handle,
                op,
                file: file.clone(),
                ok: answered.is_ok(),
            });
        }
    }
}

impl Vfs for Handles {
    fn open(&self, path: &Path, mode: OpenMode) -> io::Result<Box<dyn VfsFile>> {
        let inner = self.inner.open(path, mode)?;
        let of_the_value_log = path.extension().is_some_and(|extension| extension == "val");
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned());
        Ok(Box::new(Numbered {
            inner,
            handle: self.next.fetch_add(1, Ordering::Relaxed),
            file: name.filter(|_| of_the_value_log),
            seen: self.seen.clone(),
        }))
    }

    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        self.inner.rename(from, to)
    }

    fn hard_link(&self, original: &Path, link: &Path) -> io::Result<()> {
        self.inner.hard_link(original, link)
    }

    fn remove_file(&self, path: &Path) -> io::Result<()> {
        self.inner.remove_file(path)
    }

    fn create_dir(&self, path: &Path) -> io::Result<()> {
        self.inner.create_dir(path)
    }

    fn remove_dir(&self, path: &Path) -> io::Result<()> {
        self.inner.remove_dir(path)
    }

    fn read_dir(&self, path: &Path) -> io::Result<Vec<DirEntry>> {
        self.inner.read_dir(path)
    }

    fn sync_dir(&self, path: &Path) -> io::Result<()> {
        self.inner.sync_dir(path)
    }

    fn lock(&self, path: &Path) -> io::Result<Box<dyn VfsLock>> {
        self.inner.lock(path)
    }

    fn lock_existing(&self, path: &Path) -> io::Result<Box<dyn VfsLock>> {
        self.inner.lock_existing(path)
    }
}

impl VfsFile for Numbered {
    fn read_at(&self, buf: &mut [u8], offset: u64) -> io::Result<usize> {
        self.inner.read_at(buf, offset)
    }

    fn write_at(&self, buf: &[u8], offset: u64) -> io::Result<()> {
        self.inner.write_at(buf, offset)
    }

    fn append(&self, buf: &[u8]) -> io::Result<u64> {
        let answered = self.inner.append(buf);
        self.note("append", &answered);
        answered
    }

    fn sync_data(&self) -> io::Result<()> {
        let answered = self.inner.sync_data();
        self.note("sync", &answered);
        answered
    }

    fn sync_all(&self) -> io::Result<()> {
        let answered = self.inner.sync_all();
        self.note("sync", &answered);
        answered
    }

    fn len(&self) -> io::Result<u64> {
        self.inner.len()
    }

    fn set_len(&self, len: u64) -> io::Result<()> {
        self.inner.set_len(len)
    }
}
