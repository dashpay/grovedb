//! Every entry of the subtree directory, as a tool that looks at a whole
//! database reads it through the facade (`Operation::entries`).

use std::sync::Arc;

use lignin_io::sim::{FaultRule, OpSet, SimConfig, SimVfs};

use super::{halt_background, numbered, options, plant, subtree, temp, Merk, Writes};
use crate::{
    db::Db,
    snapshot::{Listed, ReadError},
    txn::Subtree,
    Options,
};

/// Every entry the walk gives, until its end.
fn listed(db: &Db) -> Vec<Listed> {
    let snapshot = db.snapshot();
    let operation = snapshot.operation();
    let mut walk = operation.entries().expect("the directory");
    let mut listed = Vec::new();
    while let Some(entry) = walk.next().expect("an entry") {
        listed.push(entry);
    }
    assert!(format!("{walk:?}").contains("EntryWalk"));
    listed
}

/// The walk gives every entry in the order of the prefixes, with its tree
/// type, its root and its host; and the entry of a subtree below one that
/// was dropped, which the reclaimer has not taken yet, as not in use.
#[test]
fn every_entry_is_listed_in_the_order_of_its_prefix_with_whether_it_is_in_use() {
    let dir = temp();
    let db = Db::open(dir.path(), &options()).expect("a new database");
    // Nothing is reclaimed but what the test asks for.
    halt_background(&db);
    assert!(listed(&db).is_empty());

    plant(&db, subtree(9), Merk::sized(1, 5, 10));
    let host = Merk::sized(2, 3, 10);
    let host_key = host.items()[0].0.clone();
    Writes {
        enter: vec![
            (subtree(4), None),
            (subtree(6), Some((subtree(4), host_key.clone()))),
        ],
        merks: vec![
            (subtree(4), host.hosting(&host_key, subtree(6))),
            (subtree(6), Merk::sized(3, 2, 10)),
        ],
        ..Writes::default()
    }
    .commit(&db);

    let all = listed(&db);
    let subtrees: Vec<Subtree> = all.iter().map(|listed| listed.subtree).collect();
    assert_eq!(subtrees, [subtree(4), subtree(6), subtree(9)]);
    assert!(all.iter().all(|listed| listed.live));
    assert!(all[0].entry.host().is_none());
    let hosted = all[1].entry.host().expect("a host");
    assert_eq!(
        (hosted.parent, hosted.key),
        (subtree(4).as_bytes(), &host_key[..])
    );
    assert_eq!(
        all[2].entry.root().expect("a root").key,
        Merk::sized(1, 5, 10).items()[2].0
    );

    // The drop takes the entry of the host's Merk at once. The entry of the
    // Merk below it stays until the reclaimer takes it, and is not in use.
    Writes {
        drop: vec![subtree(4)],
        ..Writes::default()
    }
    .commit(&db);
    let after = listed(&db);
    let seen: Vec<(Subtree, bool)> = after.iter().map(|l| (l.subtree, l.live)).collect();
    assert_eq!(seen, [(subtree(6), false), (subtree(9), true)]);
    // What is not in use is not opened.
    let snapshot = db.snapshot();
    let operation = snapshot.operation();
    assert!(operation.open(subtree(6)).expect("a lookup").is_none());
    drop(operation);
    drop(snapshot);

    db.reclaim();
    let reclaimed: Vec<Subtree> = listed(&db).iter().map(|l| l.subtree).collect();
    assert_eq!(reclaimed, [subtree(9)]);
}

/// A page of the directory that cannot be read ends the walk with an
/// error, and not as if the directory ended there: a call after the error
/// fails while the page is not read, and once it is, the walk goes on
/// after the last entry it gave, to the end.
#[test]
fn a_page_of_the_directory_that_is_not_read_is_an_error_and_not_the_end() {
    const SUBTREES: u32 = 1_200;
    let fs = SimVfs::new(SimConfig::new(7));
    let options = Options {
        vfs: Arc::new(fs.clone()),
        ..options()
    };
    let db = Db::open("/db", &options).expect("a new database");
    for first in (0..SUBTREES).step_by(300) {
        let enter = (first..first + 300).map(|number| (numbered(number), None));
        Writes {
            enter: enter.collect(),
            ..Writes::default()
        }
        .commit(&db);
    }
    db.close().expect("a close writes every page back");

    // A new session holds no page of the directory's leaves in its pool.
    let db = Db::open("/db", &options).expect("the database");
    assert_eq!(listed(&db).len(), SUBTREES as usize);
    drop(db);
    let db = Db::open("/db", &options).expect("the database");
    let snapshot = db.snapshot();
    let operation = snapshot.operation();
    let mut walk = operation.entries().expect("the directory");
    let first = walk.next().expect("the first page is read");
    assert_eq!(first.expect("an entry").subtree, numbered(0));

    fs.add_rule(
        FaultRule::new(lignin_io::sim::Fault::Eio)
            .on(OpSet::READS)
            .forever(),
    );
    let mut seen = vec![numbered(0)];
    let failed = loop {
        match walk.next() {
            Ok(Some(listed)) => seen.push(listed.subtree),
            Ok(None) => panic!(
                "the walk ended after {} entries with a page not read",
                seen.len()
            ),
            Err(error) => break error,
        }
    };
    assert!(
        seen.len() < SUBTREES as usize,
        "every entry was on the pages read"
    );
    assert!(
        matches!(failed, ReadError::Directory(_) | ReadError::Flat(_)),
        "{failed:?}"
    );
    // While the page is not read, the walk does not end.
    assert!(
        walk.next().is_err(),
        "a call after the error with the page still not read"
    );
    fs.clear_rules();
    while let Some(listed) = walk.next().expect("every page is read") {
        seen.push(listed.subtree);
    }
    let every: Vec<Subtree> = (0..SUBTREES).map(numbered).collect();
    assert_eq!(seen, every, "each entry once, in order");
    drop(walk);
    drop(operation);
    drop(snapshot);
    db.unchecked_at_close();
    drop(db);
}
