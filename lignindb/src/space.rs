//! The room on the device: what an error says about it, and a reserve
//! that is kept for the day it is used up.
//!
//! - [`out_of_space`] says whether an error of this crate was caused by a
//!   device that is full.
//! - [`Reserve`] is a file of bytes that are no data, in the directory of
//!   a database: room that is given back when nothing else can be.
//!
//! `docs/lignindb/disk-full-and-backup.md` says what the engine does when
//! a device fills, in which order it gets room back, and what an operator
//! does.
//!
//! # Why a reserve
//!
//! **On a device with no free byte the engine gives no room back by
//! itself.** What it holds that is dead is the log before the table
//! checkpoint that could be taken, the segments whose blocks no chain
//! points to any more, and the value-log files that nothing points into.
//! Each of them goes only after something was written: a table checkpoint
//! and the manifest that names it, for a log file and for a segment; the
//! copies of the blocks that are still live, for a segment that is not all
//! dead; and the record that ends a log file whose last append failed,
//! before the next commit can be taken at all (`transactions.md` CM9
//! item 4). So a device that fills while a database holds much that is
//! dead stays full, with a writer that is refused, until somebody outside
//! the engine makes room.
//!
//! A reserve is that room, set aside while there was some. It is a file
//! and not a number: bytes that the file system has given out are there
//! when they are given back, whatever else filled the device meanwhile,
//! and no question to the file system about its free space, which
//! `lignin_io` does not ask, is needed.
//!
//! # The order when a device is full
//!
//! 1. **The writer fails, and nothing of it is lost or half done.** A
//!    commit whose group does not reach the log has not happened
//!    ([`CommitError::Failed`](crate::commit::CommitError::Failed)), and
//!    the commits after it are refused with their transactions whole
//!    ([`CommitError::NotReady`](crate::commit::CommitError::NotReady))
//!    until the log's failed bytes are cut off. Readers go on.
//! 2. **The reserve is released** ([`Reserve::release`]).
//! 3. **Write-back takes its turn**: the cut of the log, the pages that
//!    wait, and, where one is due, a table checkpoint, after which the
//!    log files it covers go.
//! 4. **The cleaner takes its passes**: the segments and the value-log
//!    files that are all dead go at once, and the live blocks of the
//!    others are moved while the dead space is above its target. Each
//!    deletion waits for a table checkpoint, which the cleaner asks for,
//!    and with which the log before it goes as well.
//! 5. **The writer is taken again** as soon as the cut is made, which is
//!    after step 3: by the session that was refused, unless a sync of the
//!    log or of a segment was refused while the device was full. Then the
//!    session has ended, [`Db::reopen`](crate::Db::reopen) says so, and
//!    the writer is taken once the database was closed and opened again,
//!    which needs room as well.
//! 6. **The reserve is filled again** ([`Reserve::fill`]) once a table
//!    checkpoint was installed and the cleaner is within its target, and
//!    not before: a reserve that is taken back out of the room the cleaner
//!    has just made leaves the device full again.
//!
//! If the device is full of data that is live, steps 3 and 4 give back
//! less than the writer needs, and the reserve cannot be filled again:
//! the engine then runs on what the operator frees.
//!
//! **A session that keeps a reserve does steps 2 and 6 by itself**
//! ([`Options::reserve`](crate::Options::reserve)): it releases the
//! reserve when the device refuses it something for room, and fills it
//! again from the cleaner's thread. `disk-full-and-backup.md` says when,
//! in DF13. A session that keeps none makes no call of a reserve, and
//! whoever opens the database can keep one by the calls of [`Reserve`].

use std::{
    collections::hash_map::RandomState,
    error::Error,
    hash::BuildHasher,
    io::{self, ErrorKind},
    path::{Path, PathBuf},
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use lignin_io::{Create, OpenMode, Vfs, VfsFile};

#[cfg(all(test, not(loom)))]
mod tests;

/// Whether `error`, or an error it was made of, is the file layer's "no
/// space left": the device is full, or a quota on it is used up
/// (`lignin_io::vfs`, "Errors").
///
/// Every error of this crate that the file layer caused keeps that error
/// as its source, down to the [`io::Error`], and this follows the sources.
/// It says `false` of an error that tells its reason only as text, as
/// [`Reopen`](crate::db::Reopen) does: such an error is about a session
/// that has ended, whatever ended it.
///
/// It says what caused the error, and not that the device is full now. A
/// sync that was refused stays failed for its session, and is reported
/// with the error it had for as long as the session lives. Whether a
/// session goes on once there is room is not read from an error:
/// [`Db::reopen`](crate::Db::reopen) says it.
pub fn out_of_space(error: &(dyn Error + 'static)) -> bool {
    let mut next = Some(error);
    while let Some(error) = next {
        let full = error.downcast_ref::<io::Error>().is_some_and(|error| {
            matches!(
                error.kind(),
                ErrorKind::StorageFull | ErrorKind::QuotaExceeded
            )
        });
        if full {
            return true;
        }
        next = error.source();
    }
    false
}

/// How many bytes of a reserve are written at a time.
const CHUNK_LEN: usize = 1 << 20;

/// The reserve of one database directory: the file [`Self::FILE_NAME`] in
/// it, whose bytes are no data.
///
/// See the [module documentation](self) for what it is for and when it is
/// released and filled. Nothing of a database depends on it: it can be
/// missing or removed at any moment, by anyone, and a crash can take it
/// or bring it back.
///
/// **The calls of a reserve are for one caller at a time**: whoever has
/// the database open. They are not coordinated with each other. Of two
/// fills at once, or a fill beside a release, one can take away the file
/// the other is writing or give its name to another, and a fill can then
/// return with no reserve left, or with one that is short or not durable.
/// A session that keeps a reserve
/// ([`Options::reserve`](crate::Options::reserve)) is that caller from
/// its open to its close, and makes its calls one at a time.
#[derive(Debug, Clone)]
pub struct Reserve {
    vfs: Arc<dyn Vfs>,
    dir: PathBuf,
    path: PathBuf,
    unfinished: PathBuf,
}

impl Reserve {
    /// The name of the file in the database directory.
    pub const FILE_NAME: &'static str = "RESERVE";

    /// The name a reserve is written under until its bytes are durable.
    /// A file of this name is what a fill left that was cut short: it is
    /// no reserve, and the next fill or release removes it.
    pub const UNFINISHED_FILE_NAME: &'static str = "RESERVE.new";

    /// The reserve of the database directory `dir` of `vfs`. Nothing is
    /// read or made.
    pub fn of(vfs: Arc<dyn Vfs>, dir: &Path) -> Self {
        Self {
            vfs,
            dir: dir.to_path_buf(),
            path: dir.join(Self::FILE_NAME),
            unfinished: dir.join(Self::UNFINISHED_FILE_NAME),
        }
    }

    /// The file of the reserve.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The bytes the reserve holds now: 0 if there is none. What a fill
    /// that was cut short left is not counted.
    pub fn held(&self) -> io::Result<u64> {
        match self.vfs.open(&self.path, OpenMode::Read) {
            Ok(file) => file.len(),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(0),
            Err(error) => Err(error),
        }
    }

    /// Brings the reserve to `len` bytes and returns how many were
    /// written. A reserve that holds as many already is left as it is, and
    /// one that holds fewer, or more, is written anew.
    ///
    /// The bytes are written, and not only promised: a file that is grown
    /// to a length holds no room, and bytes that are all alike hold none
    /// on a file system that packs what it stores, or that stores a block
    /// once however many files have it. No block of a reserve comes twice
    /// in it. Each fill begins its stream at a place of its own, drawn
    /// among 2^64, so two reserves have a block in common only where
    /// those places fall within a reserve's length of each other.
    ///
    /// **When this returns `Ok` for a `len` above 0, the reserve is
    /// durable**, also where it wrote nothing. (A fill to 0 is a release,
    /// and makes nothing durable: [`Self::release`].) The file is written
    /// under another name
    /// ([`Self::UNFINISHED_FILE_NAME`]), made durable, and only then given
    /// its own name, which is made durable last. So a file of the
    /// reserve's name had all its bytes on the device before it was called
    /// so, whatever cut the fill that made it short, and a fill that finds
    /// one of the right length has only its name to make durable.
    ///
    /// If the device has no room for it, the call fails with the file
    /// layer's error ([`out_of_space`]) and leaves no reserve, also where
    /// there was a smaller one: what it had written is removed, as far as
    /// the file layer lets it, so that a failed fill takes no room.
    pub fn fill(&self, len: u64) -> io::Result<u64> {
        if len > 0 && self.held()? == len {
            self.remove_unfinished()?;
            self.vfs.sync_dir(&self.dir)?;
            return Ok(0);
        }
        self.release()?;
        if len == 0 {
            return Ok(0);
        }
        // From here on the file is this call's to take away. One that
        // somebody made under the name since the release is not.
        let file = (self.vfs).open(&self.unfinished, OpenMode::Append(Create::New))?;
        let named = write(&*file, len)
            .and_then(|()| self.vfs.rename(&self.unfinished, &self.path))
            .map_err(|error| (error, &self.unfinished));
        let durable = named.and_then(|()| {
            let synced = self.vfs.sync_dir(&self.dir);
            synced.map_err(|error| (error, &self.path))
        });
        durable.map(|()| len).map_err(|(error, left)| {
            let _ = self.vfs.remove_file(left);
            error
        })
    }

    /// Gives the room back: removes the file, and returns the bytes it
    /// held. With no reserve there is nothing to give back, which is no
    /// error. What a fill that was cut short left goes as well, and is
    /// not counted.
    ///
    /// Nothing is synced: the room is back when the name is gone, and a
    /// sync can need room that is not there. A crash can bring the file
    /// back, which is a reserve again.
    pub fn release(&self) -> io::Result<u64> {
        self.remove_unfinished()?;
        let held = self.held()?;
        match self.vfs.remove_file(&self.path) {
            Ok(()) => Ok(held),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(0),
            Err(error) => Err(error),
        }
    }

    /// Takes the two files of a reserve off the device, each by itself:
    /// what a fill that was cut short left
    /// ([`Self::UNFINISHED_FILE_NAME`]) and the reserve
    /// ([`Self::FILE_NAME`]), in this order. For each it returns the
    /// bytes that the file held if it was removed, `None` if there was no
    /// file of that name, and the file layer's error if the length of the
    /// file could not be read or the file could not be removed. Nothing
    /// is synced, as in [`Self::release`].
    pub fn take_away(&self) -> [io::Result<Option<u64>>; 2] {
        [&self.unfinished, &self.path].map(|path| self.take(path))
    }

    fn take(&self, path: &Path) -> io::Result<Option<u64>> {
        let len = match self.vfs.open(path, OpenMode::Read) {
            Ok(file) => file.len()?,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        match self.vfs.remove_file(path) {
            Ok(()) => Ok(Some(len)),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// Removes what a fill that was cut short left, if anything.
    fn remove_unfinished(&self) -> io::Result<()> {
        match self.vfs.remove_file(&self.unfinished) {
            Err(error) if error.kind() != ErrorKind::NotFound => Err(error),
            _ => Ok(()),
        }
    }
}

/// Appends `len` bytes of a stream of its own to `file` and makes them
/// durable.
fn write(file: &dyn VfsFile, len: u64) -> io::Result<()> {
    // Where this fill's stream begins. On Linux and macOS the keys of a
    // `RandomState` come from the operating system, once for a thread,
    // and are counted on for each one that is made. The time is hashed
    // with them for a platform that has no such keys.
    let now = SystemTime::now().duration_since(UNIX_EPOCH);
    let seed = RandomState::new().hash_one((len, now.map_or(0, |since| since.as_nanos())));
    let mut chunk = vec![0; CHUNK_LEN];
    let mut at = 0;
    while at < len {
        let part = (len - at).min(CHUNK_LEN as u64) as usize;
        noise(seed, at, &mut chunk[..part]);
        file.append(&chunk[..part])?;
        at += part as u64;
    }
    file.sync_all()
}

/// Fills `out` with the bytes at the offset `at`, a multiple of 8, of the
/// stream of `seed`: no file system packs them, and no word of 8 bytes
/// comes twice in one stream.
fn noise(seed: u64, at: u64, out: &mut [u8]) {
    const STEP: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut state = seed.wrapping_add((at / 8).wrapping_mul(STEP));
    for word in out.chunks_mut(8) {
        // SplitMix64, as `lignin_io::sim` draws its numbers: each word is
        // a one-to-one function of its place in the stream.
        state = state.wrapping_add(STEP);
        let mut mixed = state;
        mixed = (mixed ^ (mixed >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        mixed = (mixed ^ (mixed >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        mixed ^= mixed >> 31;
        word.copy_from_slice(&mixed.to_le_bytes()[..word.len()]);
    }
}
