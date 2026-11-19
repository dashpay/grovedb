//! The operating system's files with the syncs of files and directories
//! skipped: the file system of a test store whose tests are not about what
//! survives a crash. On a disk the syncs are most of what such tests would
//! wait for.

use std::{io, path::Path};

use lignin_io::{DirEntry, OpenMode, StdVfs, Vfs, VfsFile, VfsLock};

/// [`StdVfs`], except that a sync of a file or of a directory does nothing.
/// When the operating system writes the files out is its own business.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoSync;

impl Vfs for NoSync {
    fn open(&self, path: &Path, mode: OpenMode) -> io::Result<Box<dyn VfsFile>> {
        Ok(Box::new(NoSyncFile(StdVfs.open(path, mode)?)))
    }

    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        StdVfs.rename(from, to)
    }

    fn hard_link(&self, original: &Path, link: &Path) -> io::Result<()> {
        StdVfs.hard_link(original, link)
    }

    fn remove_file(&self, path: &Path) -> io::Result<()> {
        StdVfs.remove_file(path)
    }

    fn create_dir(&self, path: &Path) -> io::Result<()> {
        StdVfs.create_dir(path)
    }

    fn remove_dir(&self, path: &Path) -> io::Result<()> {
        StdVfs.remove_dir(path)
    }

    fn read_dir(&self, path: &Path) -> io::Result<Vec<DirEntry>> {
        StdVfs.read_dir(path)
    }

    fn sync_dir(&self, _path: &Path) -> io::Result<()> {
        Ok(())
    }

    fn lock(&self, path: &Path) -> io::Result<Box<dyn VfsLock>> {
        StdVfs.lock(path)
    }
}

#[derive(Debug)]
struct NoSyncFile(Box<dyn VfsFile>);

impl VfsFile for NoSyncFile {
    fn read_at(&self, buf: &mut [u8], offset: u64) -> io::Result<usize> {
        self.0.read_at(buf, offset)
    }

    fn read_exact_at(&self, buf: &mut [u8], offset: u64) -> io::Result<()> {
        self.0.read_exact_at(buf, offset)
    }

    fn write_at(&self, buf: &[u8], offset: u64) -> io::Result<()> {
        self.0.write_at(buf, offset)
    }

    fn append(&self, buf: &[u8]) -> io::Result<u64> {
        self.0.append(buf)
    }

    fn sync_data(&self) -> io::Result<()> {
        Ok(())
    }

    fn sync_all(&self) -> io::Result<()> {
        Ok(())
    }

    fn len(&self) -> io::Result<u64> {
        self.0.len()
    }

    fn set_len(&self, len: u64) -> io::Result<()> {
        self.0.set_len(len)
    }
}
