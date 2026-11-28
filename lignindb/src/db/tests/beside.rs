//! A checkpoint that is opened beside the database it was made of, which
//! goes on (`transactions.md` CK5, CK7): the two share files as hard
//! links, and the live database appends to the log file that the
//! checkpoint's manifest names closed at the length it was sampled at.

use std::sync::atomic::AtomicU32;

use super::*;
use crate::checkpoint::CheckpointOptions;

/// **A checkpoint opens while the live database appends to the log file
/// the two share.** Bytes appended to a shared file past the length the
/// checkpoint's manifest gives it are no part of the checkpoint (CK5), so
/// an open of the checkpoint reads the file to that length and no
/// further, whatever the file's length is by then and however it grows
/// during the open.
///
/// The live database makes a commit before every look that the open of
/// the checkpoint takes at its log file: the file is longer at each look
/// than at the one before.
#[test]
fn a_checkpoint_opens_while_the_live_database_appends_to_the_log_they_share() {
    let dir = temp();
    let vfs = Watched::default();
    let options = Options {
        vfs: Arc::new(vfs.clone()),
        ..options()
    };
    let live = open_at_rest(dir.path().join("live"), &options).expect("a new database");
    let live = Arc::new(live);
    plant(&live, subtree(1), Merk::sized(b'k', 10, 20));
    let target = dir.path().join("checkpoint");
    let created = live
        .checkpoint(&target, CheckpointOptions::default())
        .unwrap_or_else(|error| panic!("the checkpoint: {error}"));
    assert_eq!(created.version, live.version());
    assert!(created.linked > 0 && created.copied == 0, "{created:?}");

    // The log file that the two share: the last one the checkpoint has.
    let shared = (std::fs::read_dir(target.join("wal")).expect("the checkpoint's log"))
        .map(|entry| entry.expect("an entry").path())
        .max()
        .expect("a log file");
    let len = |path: &Path| std::fs::metadata(path).expect("the file").len();
    let sampled = len(&shared);

    let looks = Arc::new(AtomicU32::new(0));
    let (beside, counted) = (live.clone(), looks.clone());
    vfs.before_every_read_of_a_log_file_in(&target, move || {
        let look = counted.fetch_add(1, Ordering::Relaxed);
        plant(&beside, numbered(look), Merk::sized(b'k', 2, 20));
    });
    let opened = Db::open(&target, &options);
    let looks = looks.load(Ordering::Relaxed);
    assert!(looks >= 2, "the open looked at its log {looks} times");
    assert!(
        len(&shared) > sampled,
        "the commits beside the open appended to the file the two share"
    );
    assert_eq!(
        live.version().get(),
        created.version.get() + u64::from(looks)
    );
    let opened = opened.unwrap_or_else(|error| {
        panic!(
            "the checkpoint is opened, with {looks} commits of the live database beside: {error}"
        )
    });

    // The checkpoint is the version it was made at, and nothing of what
    // the live database committed since.
    assert_eq!(opened.version(), created.version);
    let snapshot = opened.snapshot();
    assert_eq!(
        seen(&snapshot.operation(), subtree(1), b"k"),
        Some((10, vec![]))
    );
    assert_eq!(seen(&snapshot.operation(), numbered(0), b"k"), None);
    drop(snapshot);
    opened.close().expect("the checkpoint is closed");
}
