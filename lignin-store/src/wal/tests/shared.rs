//! A log file that two database directories share. A checkpoint links the
//! file the live database is still appending to, and its manifest names
//! the file closed where the log ended at the sample (CK5 of
//! `transactions.md`). An open of the checkpoint looks at the file twice,
//! with the reader and at the adoption, and the live database appends
//! between the two.

use lignin_format::manifest::LastFileIds;
use lignin_io::{sim::SimConfig, Create, OpenMode, SimVfs};

use super::*;

const CHECKPOINT: &str = "/checkpoint";

fn checkpoint_dir() -> &'static Path {
    Path::new(CHECKPOINT)
}

/// A live database whose log is in its third file and durable to its end,
/// with the items it holds.
fn live(rng: &mut Rng) -> (SimVfs, Db, Vec<Item>) {
    let (fs, mut db) = sim_db(SimConfig::new(7), small_files(1500));
    let mut items = Vec::new();
    let mut in_the_last = 0;
    while in_the_last < 2 {
        if db.log.is_due() {
            db.roll().expect("a roll");
        }
        let item = db.commit(rng).expect("a commit");
        if item.at.file() >= file_id(3) {
            in_the_last += 1;
        }
        items.push(item);
    }
    db.log.sync().expect("a sync");
    (fs, db, items)
}

/// The last id that the manifest of `db` records for a log file.
fn last_log_id(db: &Db) -> Option<FileId> {
    let manifest = db.manifest.manifest().expect("a manifest");
    manifest.last_file_ids().wal
}

/// Makes the directory of a checkpoint whose log ends at `sampled`: a hard
/// link to each file of the live log, and a manifest that names each one
/// closed where the sample found it to end. The last file, which the live
/// database goes on appending to, is named in the state `last`.
fn link(fs: &SimVfs, live: &Db, sampled: &LogEnd, last: FileState) -> ManifestStore {
    let dir = checkpoint_dir();
    fs.create_dir(dir).expect("the directory");
    fs.create_dir(&FileNames::WAL.dir(dir)).expect("wal/");
    let shared = sampled.files().last().expect("a file").file();
    let named = sampled
        .files()
        .iter()
        .map(|file| {
            let id = file.file();
            let original = FileNames::WAL.path(sim::db_dir(), id);
            fs.hard_link(&original, &FileNames::WAL.path(dir, id))
                .expect("a link");
            let state = if id == shared {
                last
            } else {
                FileState::Closed
            };
            LiveFile::new(FileKind::Wal, id, state, file.valid_len()).expect("an entry")
        })
        .collect();
    let ids = LastFileIds {
        wal: last_log_id(live),
        ..LastFileIds::default()
    };
    let manifest = Manifest::new(PageSize::DEFAULT, ids, None, named).expect("a manifest");
    ManifestStore::create(sim::shared(fs), dir, manifest).expect("a manifest")
}

/// The reader's pass over the checkpoint's log.
fn read_checkpoint(fs: &SimVfs, manifest: &ManifestStore) -> LogEnd {
    let named = manifest.manifest().expect("a manifest");
    LogReader::new(sim::shared(fs), checkpoint_dir(), named)
        .into_end()
        .expect("the log reads")
}

/// The length of the log file `file`, by the name the checkpoint has for
/// it.
fn len_in_checkpoint(fs: &SimVfs, file: FileId) -> u64 {
    let path = FileNames::WAL.path(checkpoint_dir(), file);
    let file = fs.open(&path, OpenMode::Read).expect("the file opens");
    file.len().expect("a length")
}

#[test]
fn a_checkpoints_log_opens_while_the_live_database_appends_to_the_file_they_share() {
    let mut rng = Rng(171);
    let (fs, live, items) = live(&mut rng);
    let (_, sampled) = live.read();
    let shared = *sampled.files().last().unwrap();
    assert_eq!(sampled.place(), live.log.end());
    let manifest = link(&fs, &live, &sampled, FileState::Closed);
    let named_len = shared.valid_len();

    // The live database commits, and then the first look: the file is
    // longer than it is named, and the log ends in it where it is named.
    let first = live.commit(&mut rng).unwrap();
    assert_eq!(Some(first.at), sampled.place());
    assert!(first.end.offset() > named_len);
    let end = read_checkpoint(&fs, &manifest);
    assert_eq!(end.place(), sampled.place());
    assert_eq!(end.files().last().unwrap().file_len(), first.end.offset());

    // The live database commits again: the file grows under both of its
    // names.
    let second = live.commit(&mut rng).unwrap();
    assert_eq!(len_in_checkpoint(&fs, shared.file()), second.end.offset());
    assert!(second.end.offset() > first.end.offset());

    // The second look. The file is named closed, so the log ends at the
    // named length whatever lies behind it.
    let log = LogStore::open(&manifest, &end, small_files(1500))
        .unwrap_or_else(|error| panic!("the checkpoint's log does not open: {error}"));
    assert_eq!(log.end(), sampled.place());
    assert_eq!(log.latest(), sampled.latest());
    assert_eq!(log.durable().place, sampled.place());

    // The checkpoint goes on in a file of its own, and the file it shares
    // stays closed where it was named.
    let mut checkpoint = Db {
        vfs: sim::shared(&fs),
        dir: checkpoint_dir().to_path_buf(),
        manifest,
        log,
    };
    assert!(checkpoint.roll().unwrap());
    let own = checkpoint.commit(&mut rng).unwrap();
    assert!(Some(own.at.file()) > last_log_id(&live));
    assert_eq!(own.version, first.version);
    let closed = log_file(shared.file().get(), FileState::Closed, named_len);
    assert!(checkpoint.named().contains(&closed));

    // Each database reads its own log: what the two had at the sample, and
    // then its own commit.
    let (read, _) = checkpoint.read();
    assert_eq!(read, [items.clone(), vec![own]].concat());
    let (read, _) = live.read();
    assert_eq!(read, [items, vec![first, second]].concat());

    // The two names are one file still: nothing was copied.
    let again = live.commit(&mut rng).unwrap();
    assert_eq!(len_in_checkpoint(&fs, shared.file()), again.end.offset());
}

#[test]
fn a_log_file_named_open_that_grew_since_it_was_read_is_still_refused() {
    let mut rng = Rng(172);
    let (fs, live, _) = live(&mut rng);
    let (_, sampled) = live.read();
    let shared = *sampled.files().last().unwrap();
    let manifest = link(&fs, &live, &sampled, FileState::Open);

    let stale = read_checkpoint(&fs, &manifest);
    assert_eq!(stale.place(), sampled.place());
    let later = live.commit(&mut rng).unwrap();
    assert_eq!(len_in_checkpoint(&fs, shared.file()), later.end.offset());

    // An open file is read to its end, and whole items behind its named
    // length are the log's. A log that opened with the end from before
    // the commit would name the file closed there, and the commit would
    // be gone.
    let error = LogStore::open(&manifest, &stale, LogOptions::default()).unwrap_err();
    assert!(
        matches!(error, LogError::WrongLogEnd { file } if file == shared.file()),
        "{error}"
    );
    let end = read_checkpoint(&fs, &manifest);
    assert_eq!(end.place(), Some(later.end));
    let log = LogStore::open(&manifest, &end, LogOptions::default()).unwrap();
    assert_eq!(log.end(), Some(later.end));
}

#[test]
fn a_log_file_named_closed_that_is_shorter_than_its_named_length_is_still_refused() {
    let mut rng = Rng(173);
    let (bytes, ends, _) = crafted_file(1, lignin_format::wal::LogStart::FIRST, 6, &mut rng);
    let named_len = ends[3];
    let named = log_file(1, FileState::Closed, named_len);
    let (fs, vfs) = crafted(&[(named, &bytes)]);
    let manifest = ManifestStore::open(vfs.clone(), sim::db_dir()).unwrap();
    let end = LogReader::new(vfs, sim::db_dir(), manifest.manifest().unwrap())
        .into_end()
        .unwrap();
    assert_eq!(end.place(), Some(place(1, named_len)));
    assert_eq!(end.files()[0].file_len(), bytes.len() as u64);

    // The file loses a byte that the manifest vouches for.
    let path = log_path(sim::db_dir(), 1);
    let file = fs.open(&path, OpenMode::Write(Create::Never)).unwrap();
    file.set_len(named_len - 1).unwrap();
    drop(file);
    let error = LogStore::open(&manifest, &end, LogOptions::default()).unwrap_err();
    assert!(
        matches!(error, LogError::WrongLogEnd { file } if file == file_id(1)),
        "{error}"
    );
}
