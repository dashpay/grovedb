//! Opening a database: what is in the directory, the first commit of a new
//! one, and where the keyspaces are.

use std::{
    ffi::{OsStr, OsString},
    io::{self, ErrorKind},
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};

use lignin_format::{
    manifest::Manifest, wal::GroupWriter, CommitVersion, FormatError, PageId, PageSize,
};
use lignin_io::{Vfs, VfsLock};
use lignin_store::{
    recovery::recover,
    segment::{read_manifest, FileNames, ManifestStore, SegmentStoreError},
    wal::SyncPolicy,
};
use lignin_tree::{
    directory::Directory,
    drop::FlatDrops,
    flat::{FlatTree, Private},
};

use super::{
    error::OpenError,
    store::{Catalog, Shared},
};
use crate::{
    options::Options,
    txn::{KeySet, Keyspaces, WritePath},
};

/// The name of the file that is locked while a handle has the database
/// open.
pub(super) const LOCK_FILE: &str = "LOCK";

/// What a look at a directory found. The look changes nothing.
#[derive(Debug)]
enum Found {
    /// A database of this engine, with the manifest in force.
    Database(Box<Manifest>),
    /// No database: no directory, or one with nothing of any engine in it.
    /// What else it holds is not the engine's, and stays.
    Nothing {
        /// Whether the directory is there.
        exists: bool,
    },
}

/// Whether an entry with this name is one that RocksDB makes in its
/// database directory and LigninDB never does.
fn is_rocksdb(name: &OsStr) -> bool {
    let Some(name) = name.to_str() else {
        return false;
    };
    matches!(name, "CURRENT" | "IDENTITY")
        || name.starts_with("MANIFEST-")
        || name.starts_with("OPTIONS-")
        || name.ends_with(".sst")
}

/// Whether an entry with this name is a directory of the engine's own
/// files.
fn is_own_dir(name: &OsStr) -> bool {
    [
        FileNames::WAL,
        FileNames::SEGMENT,
        FileNames::VALUE_LOG,
        FileNames::PAGE_TABLE,
    ]
    .iter()
    .any(|names| names.dir(Path::new("")).as_os_str() == name)
}

/// Looks at the directory `dir` and says what it holds, reading only.
///
/// A directory of another engine, a database of a format version this
/// build does not read and files of a database with no manifest are each
/// refused here, before anything is written (P-02).
fn look(vfs: &dyn Vfs, dir: &Path) -> Result<Found, OpenError> {
    let entries = match vfs.read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == ErrorKind::NotFound => {
            return Ok(Found::Nothing { exists: false });
        }
        Err(error) => return Err(OpenError::io("list", dir)(error)),
    };
    let foreign = |engine, entry: &OsStr| OpenError::OtherEngine {
        dir: dir.to_path_buf(),
        engine,
        entry: entry.to_os_string(),
    };
    if let Some(entry) = entries.iter().find(|entry| is_rocksdb(&entry.name)) {
        return Err(foreign("RocksDB", &entry.name));
    }
    match read_manifest(vfs, dir) {
        Ok(manifest) => Ok(Found::Database(Box::new(manifest))),
        Err(SegmentStoreError::NoManifest { .. }) => {
            match entries.iter().find(|entry| is_own_dir(&entry.name)) {
                Some(entry) => Err(OpenError::NoManifest {
                    dir: dir.to_path_buf(),
                    entry: entry.name.clone(),
                }),
                None => Ok(Found::Nothing { exists: true }),
            }
        }
        Err(SegmentStoreError::Format { source, .. }) => match source {
            FormatError::UnsupportedFormatVersion { found } => Err(OpenError::FormatVersion {
                dir: dir.to_path_buf(),
                found,
            }),
            FormatError::BadMagic => Err(foreign(
                "its MANIFEST is no LigninDB file",
                OsStr::new(ManifestStore::FILE_NAME),
            )),
            source => Err(OpenError::Manifest(SegmentStoreError::Format {
                path: dir.join(ManifestStore::FILE_NAME),
                offset: 0,
                source,
            })),
        },
        Err(error) => Err(OpenError::Manifest(error)),
    }
}

/// Creates the directory `dir` with every directory above it that is not
/// there, and makes its name durable. No directory is listed for it: the
/// one above a new database can hold anything.
fn create_dir(vfs: &dyn Vfs, dir: &Path) -> Result<(), OpenError> {
    // The directory above a path of one name is the one the process is
    // in: the name is made durable there.
    let parent = dir
        .parent()
        .map(|parent| match parent.as_os_str().is_empty() {
            true => Path::new("."),
            false => parent,
        });
    match vfs.create_dir(dir) {
        Ok(()) => {}
        // Another opener made it meanwhile.
        Err(error) if error.kind() == ErrorKind::AlreadyExists => return Ok(()),
        Err(error) if error.kind() == ErrorKind::NotFound => {
            // The directory above is missing too.
            let Some(parent) = parent else {
                return Err(OpenError::io("create", dir)(error));
            };
            create_dir(vfs, parent)?;
            match vfs.create_dir(dir) {
                Ok(()) => {}
                Err(error) if error.kind() == ErrorKind::AlreadyExists => return Ok(()),
                Err(error) => return Err(OpenError::io("create", dir)(error)),
            }
        }
        Err(error) => return Err(OpenError::io("create", dir)(error)),
    }
    if let Some(parent) = parent {
        vfs.sync_dir(parent)
            .map_err(OpenError::io("sync the directory", parent))?;
    }
    Ok(())
}

/// What a look at a directory with no lock held finds, of what an open
/// takes.
enum Seen {
    /// A database of this engine.
    Database,
    /// Nothing of any engine: a place for a new database. `exists` says
    /// whether the directory is there.
    Place { exists: bool },
}

/// The lock of a database directory, and the manifest it was opened or
/// made with.
pub(super) struct Opened {
    pub(super) lock: Box<dyn VfsLock>,
    pub(super) manifest: ManifestStore,
    /// Whether the directory held no database, and this open made one.
    pub(super) created: bool,
}

/// Takes the lock of the database in `dir` and opens its manifest, or
/// makes the first manifest of a new database there.
///
/// The order is the one that leaves a directory that is refused as it was:
/// look, then create the directory if a database is to be made, then lock,
/// then look again under the lock, and only then write.
///
/// **What the open is of is settled by its first look**: it opens the
/// database it saw, or makes one in the place it saw, where one is to be
/// made. It never makes a database where it first saw one: if that
/// database is gone under the lock, the directory at the path is another
/// one and is refused ([`OpenError::Replaced`]).
///
/// **A lock file is made in no directory that holds a database.** The
/// lock of a database is taken through the lock file that is there
/// (`Vfs::lock_existing`), which every database has from the open that made
/// it, before its manifest. So a directory that someone puts in the place
/// of a database at any moment of the open, another engine's say, is not
/// given a `LOCK` it did not have: with a lock file of its own it is locked
/// for a moment and refused as it is, and with none it is looked at again
/// and refused. A database that has no lock file, which a copy made
/// without it has, is refused too ([`OpenError::NoLockFile`]): that is said
/// only after the lock was asked for once more with the database in view,
/// since another open can have made the database, lock file first, between
/// the first ask and the look.
///
/// The lock file is made only in a place for a new database, where one is
/// to be made and where this open began on a place: a directory with
/// nothing of any engine in it and no lock file. If another process writes
/// into that directory between the look and the making of the lock file,
/// what the directory is refused for under the lock is
/// [`OpenError::Changed`], and it may have gained an empty `LOCK` (D-129).
pub(super) fn open_dir(dir: &Path, options: &Options) -> Result<Opened, OpenError> {
    let vfs: &Arc<dyn Vfs> = &options.vfs;
    let page_size = |manifest: &Manifest| match options.page_size {
        Some(requested) if requested != manifest.page_size() => Err(OpenError::PageSize {
            dir: dir.to_path_buf(),
            stored: manifest.page_size(),
            requested,
        }),
        _ => Ok(()),
    };
    let none = || OpenError::NoDatabase {
        dir: dir.to_path_buf(),
    };
    let replaced = || OpenError::Replaced {
        dir: dir.to_path_buf(),
    };
    // What a look with no lock held finds, if it is something this open
    // takes: a database of this engine with pages of the size that is
    // asked for, or a place for a new one.
    let looked = || match look(&**vfs, dir)? {
        Found::Database(manifest) => page_size(&manifest).map(|()| Seen::Database),
        Found::Nothing { exists } => Ok(Seen::Place { exists }),
    };
    // What the open is of is settled by its first look: it opens the
    // database that is there, or makes one in the place that is there. It
    // never makes a database where it first saw one, whatever is at the
    // path later, and it makes a lock file only in a place.
    let reopens = match looked()? {
        Seen::Database => true,
        Seen::Place { .. } if !options.create_if_missing => return Err(none()),
        Seen::Place { exists } => {
            if !exists {
                create_dir(&**vfs, dir)?;
            }
            false
        }
    };

    let lock_path = dir.join(LOCK_FILE);
    let refused = |error: io::Error| match error.kind() {
        ErrorKind::WouldBlock => OpenError::Locked {
            dir: dir.to_path_buf(),
        },
        _ => OpenError::io("lock", &lock_path)(error),
    };
    // The lock of a lock file that is there, with none made.
    let existing = || match vfs.lock_existing(&lock_path) {
        Ok(lock) => Ok(Some(lock)),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(refused(error)),
    };
    let (lock, made) = match existing()? {
        Some(lock) => (lock, false),
        // No lock file. The directory is looked at again, because it can
        // be another one than the one that was looked at, and because
        // another open can have made a database in it meanwhile.
        None => match looked()? {
            // A database has its lock file from the open that made it,
            // before its manifest: it is there now, held or free, unless
            // the database was copied without it. A lock file is made in
            // no directory that holds a database, so that one which is put
            // in the place of a database at any moment of the open gets
            // none (D-129).
            Seen::Database => match existing()? {
                Some(lock) => (lock, false),
                None => {
                    return Err(OpenError::NoLockFile {
                        dir: dir.to_path_buf(),
                    })
                }
            },
            // The database this open began on is gone.
            Seen::Place { .. } if reopens => return Err(replaced()),
            // A place for a new database, as at the first look: the lock
            // file is made here and nowhere else.
            Seen::Place { exists } => {
                if !exists {
                    create_dir(&**vfs, dir)?;
                }
                (vfs.lock(&lock_path).map_err(refused)?, true)
            }
        },
    };
    // Under the lock nothing else makes or replaces a manifest here. What
    // is refused now was not so at the look before: someone changed the
    // directory in between. If the lock file was there, the directory is
    // as that someone left it. If this open asked for it to be made, it
    // may have that file now and did not have it before.
    let changed = |error: OpenError| match made && error.left_untouched() {
        true => OpenError::Changed {
            dir: dir.to_path_buf(),
            found: error.to_string(),
        },
        false => error,
    };
    let (manifest, created) = match look(&**vfs, dir).map_err(changed)? {
        Found::Database(manifest) => {
            page_size(&manifest).map_err(changed)?;
            let store = ManifestStore::open(vfs.clone(), dir).map_err(OpenError::Manifest)?;
            (store, false)
        }
        // The database that this open began on is gone, and what is at
        // the path is another directory: no database is made in it.
        Found::Nothing { .. } if reopens => return Err(replaced()),
        Found::Nothing { .. } => {
            let size = options.page_size.unwrap_or(PageSize::DEFAULT);
            let empty = Manifest::new(size, Default::default(), None, Vec::new())
                .map_err(OpenError::create("the first manifest"))?;
            let store =
                ManifestStore::create(vfs.clone(), dir, empty).map_err(OpenError::Manifest)?;
            (store, true)
        }
    };
    if let Some(requested) = options.page_size {
        // The check the store asks for before a page is read (M18).
        (manifest.check_page_size(requested)).map_err(OpenError::Manifest)?;
    }
    Ok(Opened {
        lock,
        manifest,
        created,
    })
}

/// Recovers the store of `opened` and makes the shared parts of the
/// session, with its keyspaces named: read from the catalog, or made by
/// the first commit of a database that has none yet.
pub(super) fn open_store(
    dir: &Path,
    options: &Options,
    manifest: ManifestStore,
) -> Result<(Shared, Directory, Duration), OpenError> {
    let page_size = (manifest.manifest().map_err(OpenError::Manifest)?).page_size();
    let began = Instant::now();
    let recovered = recover(manifest, &options.recovery())?;
    let recovery = began.elapsed();
    let landed = recovered.landed();
    let troubles = Arc::new(super::store::Troubles::default());
    let told = troubles.clone();
    let store = super::store::Store {
        vfs: options.vfs.clone(),
        dir: dir.to_path_buf(),
        page_size,
        pool: recovered.pool,
        table: recovered.table,
        segments: recovered.segments,
        values: recovered.values,
        log: recovered.log,
        checkpoints: recovered.checkpoints,
        ids: recovered.allocator,
        manifest: std::sync::Mutex::new(recovered.manifest),
        flush: recovered.flush,
        stats: lignin_store::stats::Stats::new(),
        commits: crate::txn::Commits::new(landed),
        committing: crate::commit::CommitState::reporting(move |error| {
            told.write_back_failed(error);
        })
        .ending_at_a_failed_sync(),
        directories: crate::snapshot::Directories::empty(),
        catalog: std::sync::OnceLock::new(),
        recovery: recovered.report,
        dropped: Default::default(),
        reclaiming: Default::default(),
        reclaimed: Default::default(),
        signal: Default::default(),
        troubles,
        ids_released: Default::default(),
    };
    let shared = Shared::new(store);
    let directory = match landed == CommitVersion::EMPTY {
        true => create_keyspaces(&shared)?,
        false => read_catalog(&shared, landed)?,
    };
    Ok((shared, directory, recovery))
}

/// The keys of the catalog, the flat keyspace in the first page of a
/// store that says where the others are.
mod key {
    /// The layout of the catalog itself.
    pub(super) const LAYOUT: &[u8] = b"layout";
    pub(super) const DATA: &[u8] = b"data";
    pub(super) const AUX: &[u8] = b"aux";
    pub(super) const ROOTS: &[u8] = b"roots";
    pub(super) const META: &[u8] = b"meta";
    pub(super) const DIRECTORY: &[u8] = b"directory";
    pub(super) const FLAT_DROPS: &[u8] = b"flat-drops";
}

/// The layout of the catalog that this build writes and reads.
const CATALOG_LAYOUT: u8 = 1;

/// The page of the catalog: the first page a store makes.
const CATALOG_PAGE: PageId = PageId::MIN;

/// The first commit of a database: the catalog, the flat keyspace of each
/// column, the subtree directory and the keyspace of the pages that drops
/// unlink, all empty. It is made by hand, because a write transaction is
/// begun from a write path that names them.
///
/// It takes commit version 1 and is one group of the log, like every
/// commit. If the process ends before the group is in the log, the next
/// open finds a store with no commit and makes it again.
fn create_keyspaces(shared: &Shared) -> Result<Directory, OpenError> {
    let store = &*shared.store;
    let versions = &shared.versions;
    let config = store.config();
    let guard = versions.guard();
    let base = store.committed(&guard, CommitVersion::EMPTY);
    let mut pages = Private::new(base, &store.pool, WritePath::next_owner(), &store.ids);

    let tree = OpenError::create::<lignin_tree::flat::FlatError>;
    let catalog = FlatTree::create(&mut pages, config).map_err(tree("the catalog"))?;
    if catalog.root() != CATALOG_PAGE {
        return Err(OpenError::Catalog {
            what: "the first page of the store is not the catalog's",
            source: None,
        });
    }
    let mut column = |name| FlatTree::create(&mut pages, config).map_err(tree(name));
    let keyspaces = Keyspaces {
        data: column("the data column")?,
        aux: column("the aux column")?,
        roots: column("the roots column")?,
        meta: column("the meta column")?,
    };
    let directory =
        Directory::create(&mut pages, config).map_err(OpenError::create("the directory"))?;
    let flat_drops = FlatDrops::create(&mut pages, config)
        .map_err(OpenError::create("the keyspace of unlinked pages"))?;
    let named = Catalog {
        keyspaces,
        directory: *directory.tree(),
        flat_drops,
    };
    let roots = [
        (key::DATA, keyspaces.data.root()),
        (key::AUX, keyspaces.aux.root()),
        (key::ROOTS, keyspaces.roots.root()),
        (key::META, keyspaces.meta.root()),
        (key::DIRECTORY, named.directory.root()),
        (key::FLAT_DROPS, flat_drops.root_page()),
    ];
    (catalog.put(&mut pages, key::LAYOUT, &[CATALOG_LAYOUT])).map_err(tree("the catalog"))?;
    for (key, root) in roots {
        (catalog.put(&mut pages, key, &root.get().to_le_bytes())).map_err(tree("the catalog"))?;
    }

    // The commit, step by step as `crate::commit` makes one (CM3): the
    // group, write-back, the install, the key set, the handle on the
    // directory, the version.
    let commit_lock = store.committing.lock();
    let counts = store
        .values
        .step()
        .map_err(OpenError::create("the counts"))?;
    let at = store
        .log
        .next_group()
        .map_err(OpenError::create("the log"))?;
    let version = at.version();
    let mut bytes = Vec::new();
    let mut group = GroupWriter::new(version, at.address(), &mut bytes)
        .map_err(OpenError::create("the group"))?;
    pages.log_values(&mut group).map_err(tree("the group"))?;
    for page in pages.written().collect::<Vec<_>>() {
        pages
            .log_page(page, &mut group)
            .map_err(tree("the group"))?;
    }
    group.finish();
    let appended = (store.log.append_group(&at, &bytes)).map_err(OpenError::create("the log"))?;
    // What write-back is not told costs a base image where a run would
    // do, and every page of this commit is new.
    let _ = shared.write_back().committed(version, appended.at, &bytes);
    for (copy, _) in pages.into_parts().written {
        let page = copy.page();
        drop((store.pool.install(&copy, version)).map_err(OpenError::create("the install"))?);
        (store.ids.publish(page)).map_err(OpenError::create("the page ids"))?;
    }
    drop(counts);
    drop(guard);
    // The sync that the policy asks of a commit, before the publication.
    let write_back = shared.write_back();
    let synced = match store.log.options().sync {
        SyncPolicy::Never => Ok(()),
        SyncPolicy::EveryCommit => write_back.sync_log().map(drop),
        SyncPolicy::Interval(_) => write_back.sync_log_if_due().map(drop),
    };
    synced.map_err(OpenError::create("the sync of the log"))?;
    store.commits.record(version, KeySet::default());
    store.directories.set(version, directory.clone());
    (versions.publish(version)).map_err(OpenError::create("the version"))?;
    drop(commit_lock);
    let _ = store.catalog.set(named);
    Ok(directory)
}

/// Reads where the keyspaces are from the catalog, as the version the
/// database was opened at has it, and loads the handle on its subtree
/// directory. Nothing else runs yet, so the handle is one of the newest
/// version.
fn read_catalog(shared: &Shared, landed: CommitVersion) -> Result<Directory, OpenError> {
    let store = &*shared.store;
    let config = store.config();
    let guard = shared.versions.guard();
    let pages = store.committed(&guard, landed);
    let catalog = FlatTree::open(CATALOG_PAGE, config);
    let mut value = Vec::new();
    let mut read = |key: &[u8], what| -> Result<Vec<u8>, OpenError> {
        value.clear();
        match catalog.get(&pages, key, &mut value) {
            Ok(true) => Ok(value.clone()),
            Ok(false) => Err(OpenError::Catalog { what, source: None }),
            Err(error) => Err(OpenError::catalog(what)(error)),
        }
    };
    if read(key::LAYOUT, "it has no layout")? != [CATALOG_LAYOUT] {
        return Err(OpenError::Catalog {
            what: "its layout is not one this build reads",
            source: None,
        });
    }
    let mut root = |key: &[u8]| -> Result<PageId, OpenError> {
        let what = "a keyspace has no root page";
        let bytes = read(key, what)?;
        let raw = <[u8; 4]>::try_from(&bytes[..]).ok().map(u32::from_le_bytes);
        raw.and_then(PageId::new)
            .ok_or(OpenError::Catalog { what, source: None })
    };
    let keyspaces = Keyspaces {
        data: FlatTree::open(root(key::DATA)?, config),
        aux: FlatTree::open(root(key::AUX)?, config),
        roots: FlatTree::open(root(key::ROOTS)?, config),
        meta: FlatTree::open(root(key::META)?, config),
    };
    let named = Catalog {
        keyspaces,
        directory: FlatTree::open(root(key::DIRECTORY)?, config),
        flat_drops: FlatDrops::open(root(key::FLAT_DROPS)?, config),
    };
    let directory = named
        .load_directory(&pages)
        .map_err(OpenError::catalog("the subtree directory"))?;
    store.directories.set(landed, directory.clone());
    let _ = store.catalog.set(named);
    Ok(directory)
}

/// The entries of `dir` that are not the engine's, by name: what an open,
/// a recovery and a wipe leave alone (P-03).
pub(super) fn foreign_entries(vfs: &dyn Vfs, dir: &Path) -> std::io::Result<Vec<OsString>> {
    let own = |name: &OsStr| {
        is_own_dir(name)
            || name == LOCK_FILE
            || name == ManifestStore::FILE_NAME
            || name == ManifestStore::TEMP_FILE_NAME
    };
    let entries = vfs.read_dir(dir)?;
    let mut names: Vec<OsString> = (entries.into_iter())
        .map(|entry| entry.name)
        .filter(|name| !own(name))
        .collect();
    names.sort();
    Ok(names)
}
