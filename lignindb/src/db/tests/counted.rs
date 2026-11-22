//! The counts a tool reads of an open database ([`Db::stats`]): the memory
//! of the pool, the page ids, and the values read from the value log.

use super::{options, plant, subtree, temp, Merk};
use crate::db::Db;

/// The memory of the pool is told as asked for, as a mapping, and as held;
/// the page ids by their states; and every value read from the value log,
/// with its bytes, while a value in its cell is no such read.
#[test]
fn the_counts_say_what_the_pool_holds_the_ids_and_the_values_read() {
    let dir = temp();
    let options = options();
    let db = Db::open(dir.path(), &options).expect("a new database");
    let long = 3_000;
    plant(&db, subtree(1), Merk::sized(1, 4, long));
    plant(&db, subtree(2), Merk::sized(2, 4, 8));
    db.close().expect("a close");

    let db = Db::open(dir.path(), &options).expect("the database");
    let stats = db.stats();
    assert_eq!(stats.memory.capacity, options.cache_size);
    assert!(stats.memory.fixed > 0 && stats.memory.fixed <= options.cache_size);
    assert!(
        stats.memory.mapped <= stats.memory.fixed,
        "{:?}",
        stats.memory
    );
    // On the platforms the engine is built for, a pool of a megabyte of
    // frames or more is a mapping of its own.
    if cfg!(all(
        target_pointer_width = "64",
        any(target_os = "linux", target_os = "macos")
    )) {
        assert!(stats.memory.mapped > 0, "{:?}", stats.memory);
    }
    assert!(stats.memory.held >= stats.memory.fixed);
    let high = stats.ids.high_water.expect("pages are in use").get();
    assert!(stats.ids.live > 0 && stats.ids.live <= u64::from(high));
    assert_eq!(stats.store.value_reads, 0);

    let snapshot = db.snapshot();
    let operation = snapshot.operation();
    let short = super::items_of(&operation, subtree(2)).expect("entered");
    assert_eq!(short.len(), 4);
    assert_eq!(db.stats().store.value_reads, 0, "values in their cells");
    let read = super::items_of(&operation, subtree(1)).expect("entered");
    assert_eq!(read.len(), 4);
    let after = db.stats();
    assert_eq!(after.store.value_reads, 4);
    assert_eq!(after.store.value_read_bytes, 4 * long as u64);

    let report = after.to_string();
    for part in [
        "values read from the value log: 4 (12000 bytes)\n",
        "pool memory: ",
        " of them a mapping, backed as frames are first used",
        &format!("page ids: {} live", after.ids.live),
        &format!("the highest {high}\n"),
    ] {
        assert!(report.contains(part), "{part} in:\n{report}");
    }
    // The counts of the store end their line, and the next count begins
    // one of its own.
    assert!(
        report.contains("segment syncs failed: 0\npool memory: "),
        "{report}"
    );
}
