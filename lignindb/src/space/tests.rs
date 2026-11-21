use std::{error::Error, fmt, io, path::Path, sync::Arc};

use lignin_io::{
    sim::{Chance, Fault, FaultOdds, FaultRule, SimConfig, SimVfs},
    Create, OpenMode, Vfs,
};

use super::*;

const DIR: &str = "/db";

fn fs(seed: u64) -> SimVfs {
    let fs = SimVfs::new(SimConfig {
        faults: FaultOdds {
            torn_append: Chance::NEVER,
            ..FaultOdds::NONE
        },
        ..SimConfig::new(seed)
    });
    fs.create_dir(Path::new(DIR)).expect("a directory");
    fs.sync_dir(Path::new("/")).expect("its name is durable");
    fs
}

fn reserve(fs: &SimVfs) -> Reserve {
    Reserve::of(Arc::new(fs.clone()), Path::new(DIR))
}

/// An error with a source, as the errors of this crate are made.
#[derive(Debug)]
struct Wrapped(Box<dyn Error + 'static>);

impl fmt::Display for Wrapped {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("wrapped")
    }
}

impl Error for Wrapped {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(&*self.0)
    }
}

#[test]
fn an_error_that_a_full_device_caused_says_so_through_its_sources() {
    let full = || io::Error::from(io::ErrorKind::StorageFull);
    let quota = io::Error::from(io::ErrorKind::QuotaExceeded);
    let other = io::Error::from(io::ErrorKind::PermissionDenied);
    assert!(out_of_space(&full()) && out_of_space(&quota) && !out_of_space(&other));
    let nested = Wrapped(Box::new(Wrapped(Box::new(full()))));
    assert!(out_of_space(&nested));
    assert!(!out_of_space(&Wrapped(Box::new(other))));
    assert!(!out_of_space(&fmt::Error));
    // The simulator's full disk is the file layer's.
    let fs = fs(1);
    fs.add_rule(FaultRule::new(Fault::Enospc));
    let error = fs
        .open(Path::new("/db/file"), OpenMode::Write(Create::New))
        .expect_err("no room for a name");
    assert!(out_of_space(&Wrapped(Box::new(error))));
    fs.add_rule(FaultRule::new(Fault::Eio));
    let error = fs
        .open(Path::new("/db/file"), OpenMode::Write(Create::New))
        .expect_err("the device failed");
    assert!(!out_of_space(&error));
}

#[test]
fn a_reserve_is_written_held_and_given_back() {
    let fs = fs(2);
    let reserve = reserve(&fs);
    assert_eq!(reserve.path(), Path::new("/db/RESERVE"));
    assert_eq!(reserve.held().expect("no reserve"), 0);
    assert_eq!(reserve.release().expect("nothing to give back"), 0);
    assert_eq!(reserve.fill(0).expect("no reserve is asked for"), 0);
    assert!(fs
        .read_dir(Path::new(DIR))
        .expect("the directory")
        .is_empty());

    // Three chunks and a part of one.
    let len = (3 << 20) + 4321;
    assert_eq!(reserve.fill(len).expect("there is room"), len);
    assert_eq!(reserve.held().expect("the reserve"), len);
    assert_eq!(fs.used_bytes(), len);
    // It is durable when the call returns: a power cut keeps all of it.
    let image = fs.crash_image();
    let cut = image.recover(&image.lose_all()).expect("the plan fits");
    assert_eq!(
        Reserve::of(Arc::new(cut), Path::new(DIR))
            .held()
            .expect("it"),
        len
    );
    // Its bytes are written, and no two parts of it are alike.
    let file = fs.open(reserve.path(), OpenMode::Read).expect("the file");
    let mut bytes = vec![0; len as usize];
    file.read_exact_at(&mut bytes, 0).expect("its bytes");
    drop(file);
    let mut words: Vec<&[u8]> = bytes.chunks(8).collect();
    words.sort_unstable();
    words.dedup();
    assert_eq!(words.len(), bytes.len().div_ceil(8), "a part of it repeats");
    let zeros = bytes.iter().filter(|byte| **byte == 0).count();
    assert!(zeros < bytes.len() / 100, "{zeros} bytes are zero");

    // A reserve that is as long as asked is left as it is: nothing is
    // written, and its name is made durable once more.
    let before = fs.stats();
    assert_eq!(reserve.fill(len).expect("it is there"), 0);
    let after = fs.stats();
    assert_eq!(after.syncs, before.syncs + 1, "the name is synced once");
    let file = fs.open(reserve.path(), OpenMode::Read).expect("the file");
    let mut again = vec![0; len as usize];
    file.read_exact_at(&mut again, 0).expect("its bytes");
    drop(file);
    assert!(again == bytes, "the reserve was written anew");
    // One of another length is written anew, shorter or longer.
    assert_eq!(reserve.fill(1000).expect("a shorter one"), 1000);
    assert_eq!((reserve.held().expect("it"), fs.used_bytes()), (1000, 1000));
    assert_eq!(reserve.fill(5000).expect("a longer one"), 5000);
    assert_eq!(reserve.held().expect("it"), 5000);

    assert_eq!(reserve.release().expect("it is given back"), 5000);
    assert_eq!((reserve.held().expect("none"), fs.used_bytes()), (0, 0));
    assert_eq!(reserve.release().expect("twice"), 0);
    assert!(format!("{reserve:?}").contains("RESERVE"));
}

/// Two reserves of one length have no block in common, in two databases
/// and in one database at two times: a device that stores a block once
/// holds each of them whole.
#[test]
fn no_two_reserves_share_a_block() {
    let fs = fs(6);
    fs.create_dir(Path::new("/other")).expect("a directory");
    let len: u64 = 256 << 10;
    let blocks = |reserve: &Reserve| {
        let file = fs.open(reserve.path(), OpenMode::Read).expect("the file");
        let mut bytes = vec![0; len as usize];
        file.read_exact_at(&mut bytes, 0).expect("its bytes");
        let blocks: std::collections::BTreeSet<Vec<u8>> =
            bytes.chunks(512).map(<[u8]>::to_vec).collect();
        assert_eq!(blocks.len(), bytes.len() / 512, "a block comes twice");
        blocks
    };
    let one = reserve(&fs);
    let other = Reserve::of(Arc::new(fs.clone()), Path::new("/other"));
    one.fill(len).expect("room");
    other.fill(len).expect("room");
    let (first, second) = (blocks(&one), blocks(&other));
    assert!(first.is_disjoint(&second), "two reserves share a block");
    one.release().expect("it is given back");
    one.fill(len).expect("room");
    let third = blocks(&one);
    assert!(third.is_disjoint(&first) && third.is_disjoint(&second));
}

/// The room that a reserve holds is there when it is released, on a
/// device that has no byte left.
#[test]
fn a_released_reserve_is_room_on_a_device_that_is_full() {
    let fs = fs(3);
    let reserve = reserve(&fs);
    fs.set_capacity(Some(10_000));
    reserve.fill(4000).expect("there is room");
    let data = fs
        .open(Path::new("/db/data"), OpenMode::Append(Create::New))
        .expect("a file");
    data.append(&[1; 6000]).expect("the device is full now");
    let error = data.append(&[2; 1]).expect_err("no room");
    assert!(out_of_space(&error));
    // Filling a reserve that is there asks for nothing.
    assert_eq!(reserve.fill(4000).expect("it is there"), 0);

    assert_eq!(reserve.release().expect("it is given back"), 4000);
    data.append(&[2; 3000]).expect("the reserve's room");
    // There is room for a part of a reserve only: none is left, and the
    // room it took for a moment is back.
    let used = fs.used_bytes();
    let error = reserve.fill(4000).expect_err("no room for all of it");
    assert!(out_of_space(&error), "{error}");
    assert_eq!((reserve.held().expect("none"), fs.used_bytes()), (0, used));
    data.append(&[3; 1000]).expect("the last room");
    // Once enough is removed, it is filled again.
    data.set_len(2000).expect("the data is cut");
    assert_eq!(reserve.fill(4000).expect("there is room again"), 4000);
}

/// The device fails each operation of a fill that can need room, and the
/// power is cut after each: a fill that fails leaves no reserve and takes
/// no room, and a crash leaves the whole reserve or none, which the next
/// fill makes whole and durable, whatever of the fill before it was
/// durable.
#[test]
fn a_fill_that_fails_or_is_cut_leaves_nothing_that_matters() {
    let len = (2 << 20) + 100;
    let mut failed = 0;
    for skip in 0.. {
        let fs = fs(4);
        let reserve = reserve(&fs);
        fs.add_rule(FaultRule::new(Fault::Enospc).skip(skip));
        let filled = reserve.fill(len);
        if fs.stats().faults == 0 {
            assert_eq!(filled.expect("no fault"), len);
            break;
        }
        failed += 1;
        let error = filled.expect_err("the device was full");
        assert!(out_of_space(&error), "operation {skip}: {error}");
        assert_eq!(reserve.held().expect("none"), 0, "operation {skip}");
        assert_eq!(fs.used_bytes(), 0, "operation {skip}: room is taken");
        assert_eq!(reserve.fill(len).expect("there is room now"), len);
    }
    assert!(failed >= 6, "{failed} operations were failed");

    let whole = {
        let fs = fs(5);
        reserve(&fs).fill(len).expect("a fill");
        fs.stats().ops
    };
    // The directory is made before the fill begins.
    let made = fs(5).stats().ops;
    assert!(
        whole > made + 3,
        "{whole} operations, {made} before the fill"
    );
    for after in made + 1..whole {
        let fs = fs(5);
        fs.crash_at(lignin_io::sim::CrashPoint::AfterOp(after));
        let _ = reserve(&fs).fill(len);
        let image = fs.crash_image();
        // What the power leaves, and, with everything kept, what a
        // process leaves that was killed there.
        for plan in [image.lose_all(), image.keep_all(), image.sample(after)] {
            let cut = image.recover(&plan).expect("the plan fits");
            let reserve = Reserve::of(Arc::new(cut.clone()), Path::new(DIR));
            let held = reserve.held().expect("a reserve or none");
            assert!(held == 0 || held == len, "cut after {after}: {held}");
            reserve.fill(len).expect("the next fill");
            assert_eq!(reserve.held().expect("it"), len, "cut after {after}");
            // Nothing is left of the fill that was cut.
            assert_eq!(cut.used_bytes(), len, "cut after {after}");
            // The fill that returned made the reserve durable, also where
            // it found all of it written.
            let image = cut.crash_image();
            let lost = image.recover(&image.lose_all()).expect("the plan fits");
            let held = Reserve::of(Arc::new(lost), Path::new(DIR)).held();
            assert_eq!(held.expect("it"), len, "cut after {after}, and again");
        }
    }
}

/// A fill of another length is cut where a reserve was: what the power
/// leaves is the older reserve, the newer, or none, and the next fill
/// leaves one reserve of the length it was asked for and nothing else. So
/// does a fill that finds its reserve whole beside what a cut fill left.
#[test]
fn a_fill_that_was_cut_beside_an_older_reserve_leaves_one_reserve() {
    let (old, new) = (3000, (1 << 20) + 500);
    let ops = |fill_new: bool| {
        let fs = fs(8);
        reserve(&fs).fill(old).expect("the older reserve");
        if fill_new {
            reserve(&fs).fill(new).expect("the newer one");
        }
        fs.stats().ops
    };
    let (before, whole) = (ops(false), ops(true));
    assert!(whole > before + 4, "{before} and {whole} operations");
    let mut left = std::collections::BTreeSet::new();
    for after in before + 1..whole {
        let fs = fs(8);
        reserve(&fs).fill(old).expect("the older reserve");
        fs.crash_at(lignin_io::sim::CrashPoint::AfterOp(after));
        let _ = reserve(&fs).fill(new);
        let image = fs.crash_image();
        for plan in [image.lose_all(), image.keep_all(), image.sample(after)] {
            let cut = image.recover(&plan).expect("the plan fits");
            let reserve = Reserve::of(Arc::new(cut.clone()), Path::new(DIR));
            let held = reserve.held().expect("a reserve or none");
            assert!([0, old, new].contains(&held), "cut after {after}: {held}");
            left.insert(held);
            reserve.fill(old).expect("the next fill");
            assert_eq!(reserve.held().expect("it"), old, "cut after {after}");
            assert_eq!(cut.used_bytes(), old, "cut after {after}: more is left");
        }
    }
    assert_eq!(left.into_iter().collect::<Vec<_>>(), [0, old, new]);

    // The reserve is whole, and a file of the name that a fill writes
    // under is there: the fill writes nothing, and takes that file away.
    let fs = fs(9);
    let reserve = reserve(&fs);
    reserve.fill(old).expect("the reserve");
    let unfinished = Path::new(DIR).join(Reserve::UNFINISHED_FILE_NAME);
    let file = fs
        .open(&unfinished, OpenMode::Append(Create::New))
        .expect("a file");
    file.append(&[7; 900]).expect("its bytes");
    drop(file);
    assert_eq!(
        (reserve.held().expect("it"), fs.used_bytes()),
        (old, old + 900)
    );
    assert_eq!(reserve.fill(old).expect("it is there"), 0);
    assert_eq!((reserve.held().expect("it"), fs.used_bytes()), (old, old));
    // A release takes such a file away as well, and does not count it.
    let file = fs
        .open(&unfinished, OpenMode::Append(Create::New))
        .expect("a file");
    file.append(&[7; 900]).expect("its bytes");
    drop(file);
    assert_eq!(reserve.release().expect("it is given back"), old);
    assert_eq!(fs.used_bytes(), 0);
}
