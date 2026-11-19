//! The directory handles an open database keeps: one for each version that
//! a reader can still read at.

use std::{
    collections::VecDeque,
    fmt,
    sync::{PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard},
};

use lignin_format::CommitVersion;
use lignin_store::alloc::{Epoch, Pins};
use lignin_tree::directory::Directory;

/// The handles on the subtree directory that one open database keeps, by
/// version (`lignin_tree::directory`, "A handle is of the pages it was
/// loaded from").
///
/// **Every reader takes the handle of its version from here, and none loads
/// one.** What a drop left is taken out of the pages one page step at a
/// time, on the page versions that are current then, so a handle that is
/// loaded for a version that is the latest no more can take a subtree that
/// was below a dropped one for a subtree in use. The handle that is kept
/// for a version was made while that version was the newest: it is the one
/// the frames of the commit had ([`Published::directory`]), or the one the
/// open of the database loaded, alone. A copy of it is right for as long
/// as the version is read, whatever is reclaimed meanwhile.
///
/// # Which versions have a handle
///
/// | Who asks | For which version | Why the handle is there |
/// | --- | --- | --- |
/// | a snapshot, a cursor of a write transaction, the frames of a write transaction | the version it pins | a handle goes only once no pin is on its version or an older one |
/// | an operation that pins nothing | the latest version, read under its guard | a handle goes only once every guard that was held when its version was found unpinned has been released |
///
/// That is the rule the page id allocator frees a page id by
/// (`page-alloc.md` A7 to A9), with "superseded" in the place of "freed":
/// the handle of version *V* is no version's any more once *V* + 1 is
/// published. [`Self::reclaim`] asks the three questions in their order.
///
/// # Who writes
///
/// - **A commit** puts the handle of its version here before it publishes
///   the version ([`Self::set`]), under the commit lock. A reader that
///   pins the version the moment it is published finds its handle.
/// - **Whoever reclaimed what a drop left** may load the directory of the
///   latest version again and put it in the place of the kept one, under
///   the commit lock, so that the ids of the Merks that are gone for good
///   leave the handles ([`Self::set`] with the latest version).
///
/// A reader takes a read lock for one lookup, once for each version it
/// reads at, and a writer the write lock for a few instructions. No page
/// is read under either.
///
/// [`Published::directory`]: crate::commit::Published::directory
pub struct Directories {
    kept: RwLock<VecDeque<Kept>>,
}

/// The handle of one version.
struct Kept {
    version: CommitVersion,
    directory: Directory,
    /// The epoch that began after no pin was found to need the handle. It
    /// goes when every guard of an earlier epoch has been released.
    retired: Option<Epoch>,
}

impl Directories {
    /// The handles of a database that is being opened: `directory` is the
    /// handle of `version`, the version the open landed on, loaded while
    /// nothing else ran.
    pub fn new(version: CommitVersion, directory: Directory) -> Self {
        let kept = Self::empty();
        kept.set(version, directory);
        kept
    }

    /// No handle yet: the first commit of the store makes the directory.
    pub fn empty() -> Self {
        Self {
            kept: RwLock::new(VecDeque::new()),
        }
    }

    fn read(&self) -> RwLockReadGuard<'_, VecDeque<Kept>> {
        self.kept.read().unwrap_or_else(PoisonError::into_inner)
    }

    fn write(&self) -> RwLockWriteGuard<'_, VecDeque<Kept>> {
        self.kept.write().unwrap_or_else(PoisonError::into_inner)
    }

    /// Keeps `directory` as the handle of `version`.
    ///
    /// The caller holds the commit lock. `version` is the one it is about
    /// to publish, and the call comes before the publication; or it is the
    /// latest version, whose handle was loaded again under that lock. A
    /// handle that was kept for `version` or for a later one is replaced:
    /// a commit that was not published left it.
    pub fn set(&self, version: CommitVersion, directory: Directory) {
        let mut kept = self.write();
        while kept.back().is_some_and(|last| last.version >= version) {
            kept.pop_back();
        }
        kept.push_back(Kept {
            version,
            directory,
            retired: None,
        });
    }

    /// A copy of the handle of `version`, which shares the ids of the
    /// kept one, or `None` if none is kept for it.
    ///
    /// A handle is there for a version the caller pins, and for the latest
    /// version that the caller read under an operation guard it still
    /// holds.
    pub fn at(&self, version: CommitVersion) -> Option<Directory> {
        let kept = self.read();
        let found = kept.binary_search_by_key(&version, |kept| kept.version);
        found.ok().map(|index| kept[index].directory.clone())
    }

    /// How many handles are kept.
    pub fn len(&self) -> usize {
        self.read().len()
    }

    /// Whether no handle is kept.
    pub fn is_empty(&self) -> bool {
        self.read().is_empty()
    }

    /// Gives up the handles that no reader can ask for any more, by the
    /// three questions in their order (`page-alloc.md` A9), and returns
    /// how many went.
    ///
    /// The handle of a version below the oldest pin is superseded and
    /// pinned by nobody. It is stamped with the epoch that begins then, and
    /// goes once no guard of an earlier epoch is held: an operation that
    /// pins nothing and read that version as the latest took its guard
    /// before the next version was published, which was before the stamp.
    ///
    /// It is called from the [`Reclaim`](super::Reclaim) of the database,
    /// and like every reclaimer it never waits.
    pub fn reclaim<P: Pins + ?Sized>(&self, pins: &P) -> usize {
        let oldest = pins.oldest_pin();
        let unstamped = |kept: &Kept| kept.version < oldest && kept.retired.is_none();
        if !self
            .read()
            .iter()
            .any(|kept| unstamped(kept) || kept.retired.is_some())
        {
            return 0;
        }
        let epoch = pins.next_epoch();
        let guard = pins.oldest_guard();
        let mut kept = self.write();
        for kept in kept.iter_mut().filter(|kept| unstamped(kept)) {
            kept.retired = Some(epoch);
        }
        let before = kept.len();
        // The oldest go first, and a handle stays behind an older one that
        // stays: both wait for a guard of the older one's epoch.
        while kept.front().is_some_and(|first| {
            first
                .retired
                .is_some_and(|stamp| stamp.get() <= guard.get())
        }) {
            kept.pop_front();
        }
        before - kept.len()
    }
}

impl fmt::Debug for Directories {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let kept = self.read();
        f.debug_struct("Directories")
            .field("kept", &kept.len())
            .field("oldest", &kept.front().map(|kept| kept.version))
            .field("newest", &kept.back().map(|kept| kept.version))
            .finish()
    }
}
