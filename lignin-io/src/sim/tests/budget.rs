//! A capacity in bytes ([`SimVfs::set_capacity`]): what is counted, what
//! an operation needs, and that room comes back with what is removed.

use super::*;

/// A file system whose failed appends are taken back, so that a file is
/// as long as it was after each of them.
fn fs(seed: u64) -> SimVfs {
    SimVfs::new(SimConfig {
        faults: FaultOdds {
            torn_append: Chance::NEVER,
            ..FaultOdds::NONE
        },
        ..SimConfig::new(seed)
    })
}

fn appender(fs: &SimVfs, path: &str) -> Box<dyn VfsFile> {
    fs.open(Path::new(path), OpenMode::Append(Create::IfMissing))
        .expect("a file")
}

fn is_full(error: &io::Error) -> bool {
    error.kind() == ErrorKind::StorageFull && SimError::injected(error) == Some(Fault::Enospc)
}

#[test]
fn the_bytes_of_each_file_are_counted_once() {
    let fs = fs(1);
    assert_eq!((fs.capacity(), fs.used_bytes()), (None, 0));
    let one = appender(&fs, "/one");
    one.append(&[1; 700]).expect("no capacity is set");
    fs.create_dir(Path::new("/dir")).expect("a directory");
    appender(&fs, "/dir/two")
        .append(&[2; 300])
        .expect("an append");
    assert_eq!(fs.used_bytes(), 1000);

    // A second name is no second file.
    fs.hard_link(Path::new("/one"), Path::new("/dir/link"))
        .expect("a link");
    assert_eq!(fs.used_bytes(), 1000);
    // A file that is cut short holds its new length, and one that is
    // grown its hole.
    one.set_len(100).expect("it is cut");
    assert_eq!(fs.used_bytes(), 400);
    one.set_len(450).expect("it is grown");
    assert_eq!(fs.used_bytes(), 750);
}

#[test]
fn an_operation_that_the_device_has_no_room_for_fails_as_a_full_disk_does() {
    let fs = fs(2);
    fs.set_capacity(Some(1000));
    assert_eq!(fs.capacity(), Some(1000));
    let log = appender(&fs, "/log");
    log.append(&[1; 600]).expect("there is room");
    log.append(&[2; 400]).expect("the last byte fits");
    assert_eq!(fs.used_bytes(), 1000);
    let faults = fs.stats().faults;

    let error = log.append(&[3; 1]).expect_err("one byte too many");
    assert!(is_full(&error), "{error}");
    assert_eq!(
        log.len().expect("its length"),
        1000,
        "the append was taken back"
    );
    assert_eq!(fs.stats().faults, faults + 1);

    // An append of no bytes asks nothing of the device, and neither does
    // a name, a directory, a hole or a sync.
    assert_eq!(log.append(&[]).expect("no bytes"), 1000);
    let other = fs
        .open(Path::new("/other"), OpenMode::Write(Create::New))
        .expect("a name takes no room");
    fs.create_dir(Path::new("/dir"))
        .expect("a directory takes no room");
    fs.rename(Path::new("/other"), Path::new("/dir/other"))
        .expect("a rename takes no room");
    log.sync_all().expect("a sync takes no room");
    fs.sync_dir(Path::new("/")).expect("a sync takes no room");
    // The first byte under the new name fails, and a write over bytes
    // that are there needs room as well.
    let error = other.write_at(&[4; 10], 0).expect_err("no room");
    assert!(is_full(&error), "{error}");
    let error = log
        .write_at(&[5; 1], 0)
        .expect_err("a handle that appends writes nowhere else");
    assert_eq!(error.kind(), ErrorKind::InvalidInput);
    drop(log);
    let log = fs
        .open(Path::new("/log"), OpenMode::Write(Create::Never))
        .expect("the file");
    let error = log.write_at(&[5; 1], 0).expect_err("no room");
    assert!(is_full(&error), "{error}");
    // A write that is refused can leave a part of itself, as the fault
    // model has it: here at most the ten bytes and the one.
    assert!(
        (1000..=1011).contains(&fs.used_bytes()),
        "{}",
        fs.used_bytes()
    );
    drop(log);
    let log = appender(&fs, "/log");

    // A capacity that is taken away takes the limit with it.
    fs.set_capacity(None);
    log.append(&[6; 5000]).expect("no capacity");
    // One that is below what the files hold is used up.
    fs.set_capacity(Some(10));
    assert!(is_full(&log.append(&[7; 1]).expect_err("used up")));
}

#[test]
fn room_comes_back_with_what_is_removed() {
    let fs = fs(3);
    fs.set_capacity(Some(1000));
    let kept = appender(&fs, "/kept");
    kept.append(&[1; 400]).expect("there is room");
    let dead = appender(&fs, "/dead");
    dead.append(&[2; 600]).expect("there is room");
    assert!(is_full(
        &kept.append(&[3; 100]).expect_err("the device is full")
    ));

    // A file that is removed holds its bytes for as long as a handle has
    // it open, and nothing after that: no sync of the directory is waited
    // for.
    fs.remove_file(Path::new("/dead")).expect("it is removed");
    assert_eq!(fs.used_bytes(), 1000);
    assert!(is_full(
        &kept.append(&[3; 100]).expect_err("a handle has the file")
    ));
    drop(dead);
    assert_eq!(fs.used_bytes(), 400);
    kept.append(&[3; 100]).expect("there is room again");

    // A file with two names goes with the second.
    let shared = appender(&fs, "/shared");
    shared.append(&[4; 500]).expect("it fits");
    drop(shared);
    fs.hard_link(Path::new("/shared"), Path::new("/link"))
        .expect("a link");
    fs.remove_file(Path::new("/shared")).expect("one name goes");
    assert_eq!(fs.used_bytes(), 1000);
    assert!(is_full(
        &kept.append(&[5; 1]).expect_err("the link keeps the bytes")
    ));
    fs.remove_file(Path::new("/link"))
        .expect("the other name goes");
    assert_eq!(fs.used_bytes(), 500);
    kept.append(&[5; 500]).expect("there is room again");

    // A file that is cut short gives its end back.
    assert!(is_full(
        &kept.append(&[6; 1]).expect_err("the device is full")
    ));
    kept.set_len(900).expect("it is cut");
    kept.append(&[6; 100]).expect("there is room again");
    assert_eq!(fs.used_bytes(), 1000);
}

/// A rule and a capacity are two reasons for one error: the rule decides
/// where it fires, the capacity where no rule did. What a crash leaves has
/// neither.
#[test]
fn a_capacity_stands_beside_the_rules_and_is_not_carried_through_a_crash() {
    let fs = fs(4);
    fs.set_capacity(Some(100));
    let log = appender(&fs, "/log");
    fs.add_rule(FaultRule::new(Fault::Enospc).on(OpKind::Append));
    assert!(is_full(&log.append(&[1; 10]).expect_err("the rule")));
    log.append(&[1; 10])
        .expect("the rule is spent, and there is room");
    assert!(is_full(&log.append(&[2; 91]).expect_err("the capacity")));
    // A fault that is no lack of space is not the capacity's.
    fs.add_rule(FaultRule::new(Fault::Eio).on(OpKind::Append));
    let error = log.append(&[3; 200]).expect_err("the rule comes first");
    assert_eq!(SimError::injected(&error), Some(Fault::Eio));

    let image = fs.crash_image();
    let after = image.recover(&image.keep_all()).expect("the plan fits");
    assert_eq!(after.capacity(), None);
    appender(&after, "/log")
        .append(&[4; 5000])
        .expect("what a crash leaves has no capacity");
}

/// A failed write leaves what the fault model says it leaves, which can
/// be some of its sectors: the capacity is where the device refuses, and
/// no bound on what the files hold after a refusal.
#[test]
fn a_refused_write_can_leave_some_of_its_sectors() {
    let mut most = 0;
    for seed in 0..32 {
        let fs = SimVfs::new(SimConfig {
            faults: FaultOdds {
                torn_append: Chance::ALWAYS,
                ..FaultOdds::NONE
            },
            ..SimConfig::new(seed)
        });
        fs.set_capacity(Some(1024));
        let log = appender(&fs, "/log");
        let error = log.append(&[1; 4096]).expect_err("no room");
        assert!(is_full(&error), "{error}");
        let left = fs.used_bytes();
        assert!(
            left <= 4096 && left.is_multiple_of(512),
            "seed {seed}: {left}"
        );
        most = most.max(left);
    }
    assert!(most > 1024, "no refused append left more than the capacity");
}

/// The capacity bounds what an operation is given, and not what the files
/// hold: a write behind the end of a file needs its buffer, and the hole
/// before it is counted once it is there.
#[test]
fn a_hole_takes_no_room_when_it_is_made_and_counts_afterwards() {
    let fs = fs(6);
    fs.set_capacity(Some(1));
    let file = fs
        .open(Path::new("/sparse"), OpenMode::Write(Create::New))
        .expect("a name takes no room");
    file.write_at(&[1], 4096).expect("one byte fits");
    assert_eq!(fs.used_bytes(), 4097);
    let error = file.write_at(&[2], 0).expect_err("the files hold too much");
    assert!(is_full(&error), "{error}");
    file.set_len(0).expect("it is cut");
    file.write_at(&[2], 0).expect("there is room again");
}

/// A lock that is held on a file is no handle for the count: a file with
/// no name and no handle holds nothing.
#[test]
fn a_file_that_only_a_lock_holds_is_not_counted() {
    let fs = fs(7);
    appender(&fs, "/LOCK").append(&[1; 500]).expect("an append");
    let lock = fs.lock_existing(Path::new("/LOCK")).expect("the lock");
    assert_eq!(fs.used_bytes(), 500);
    fs.remove_file(Path::new("/LOCK")).expect("its name goes");
    assert_eq!(fs.used_bytes(), 0);
    drop(lock);
    assert_eq!(fs.used_bytes(), 0);
}
