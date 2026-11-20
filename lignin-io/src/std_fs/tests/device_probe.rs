//! What a set that was synced leaves on its device: an experiment on a disk
//! image, on macOS.
//!
//! No test here cuts the power, so none shows that a sync made anything
//! durable. A disk image can stand in for the device. `device_probe.sh`,
//! beside this file, makes an image with a file system of its own, mounts it,
//! and runs [`probe`] in it three times: for a base state that is synced one
//! call at a time, for changes to that state that are synced in one of the
//! [`WAYS`], after which the image file is copied at once and while it is
//! still mounted, and at last in the mounted copy, to say what is there.
//!
//! The copy holds what had reached the image file when the sync returned.
//! The experiment takes that for what the file system had written to its
//! device, and mounting the copy for a mount after a crash of the system at
//! that moment. This assumes that a disk image passes writes on as a device
//! takes them and keeps none back. A power cut can lose more than a crash of
//! the system: what the device itself had not written out. The copy does not
//! show that part, which is the flush's, so the experiment shows what a
//! set's calls put on the device, and not that the device kept it.
//!
//! ```text
//! lignin-io/src/std_fs/tests/device_probe.sh APFS together
//! ```

#![cfg(target_os = "macos")]

use super::*;

/// How the changes are synced.
const WAYS: [(&str, &str); 7] = [
    (
        "together",
        "Vfs::sync_together, with a directory that was not changed as the last item",
    ),
    (
        "together-file-last",
        "Vfs::sync_together, with the file that was appended to as the last item",
    ),
    (
        "together-read-last",
        "Vfs::sync_together, with a file that was only read as the last item",
    ),
    ("alone", "the single call of each item, one after another"),
    (
        "shared",
        "fsync of each item and one F_FULLFSYNC on the last, whatever the file system",
    ),
    ("handed", "fsync of each item and no F_FULLFSYNC"),
    ("none", "nothing"),
];

fn pattern(len: usize, seed: u8) -> Vec<u8> {
    (0..len)
        .map(|at| (at as u8).wrapping_mul(31).wrapping_add(seed))
        .collect()
}

/// Files and directories, each synced by its own call.
fn base(root: &Path) {
    let (vfs, base) = (StdVfs, root.join("base"));
    let dirs = ["", "keep", "empty", "removed", "source"].map(|dir| base.join(dir));
    for dir in &dirs {
        vfs.create_dir(dir).unwrap();
    }
    let files = [
        ("old", 10),
        ("gone", 11),
        ("keep/over", 12),
        ("keep/x", 13),
        ("replaced", 14),
        ("source/in", 15),
        ("still", 16),
    ];
    for (path, seed) in files {
        let file = StdFile::open(&base.join(path), OpenMode::Write(Create::New)).unwrap();
        file.write_at(&pattern(8192, seed), 0).unwrap();
        file.sync_all().unwrap();
    }
    for dir in dirs.iter().rev().chain([&root.to_path_buf()]) {
        vfs.sync_dir(dir).unwrap();
    }
}

/// Changes of every kind the contract has, synced as `way` says, and then
/// the copy of the image.
fn change(root: &Path, way: &str) {
    let (vfs, base) = (StdVfs, root.join("base"));
    let open = |path: &str, mode: OpenMode| StdFile::open(&base.join(path), mode).unwrap();
    let rename = |from: &str, to: &str| vfs.rename(&base.join(from), &base.join(to)).unwrap();
    let new = OpenMode::Write(Create::New);
    let existing = OpenMode::Write(Create::Never);

    let log = open("log", OpenMode::Append(Create::New));
    for seed in 1..=3 {
        log.append(&pattern(4096, seed)).unwrap();
    }
    let holed = open("holed", new);
    holed.write_at(&pattern(4096, 4), 1 << 20).unwrap();
    let grown = open("grown", new);
    grown.write_at(&pattern(100, 5), 0).unwrap();
    grown.set_len(3 << 20).unwrap();
    let old = open("old", existing);
    old.set_len(3).unwrap();
    let over = open("keep/over", existing);
    over.write_at(&pattern(4096, 6), 2048).unwrap();
    vfs.create_dir(&base.join("new")).unwrap();
    vfs.create_dir(&base.join("new/inner")).unwrap();
    let inner = open("new/inner/f", new);
    inner.write_at(&pattern(100, 7), 0).unwrap();
    let replacement = open("replaced.new", new);
    replacement.write_at(&pattern(5000, 8), 0).unwrap();
    rename("replaced.new", "replaced");
    rename("keep/x", "new/moved");
    vfs.hard_link(&base.join("log"), &base.join("new/link"))
        .unwrap();
    vfs.remove_file(&base.join("gone")).unwrap();
    vfs.remove_dir(&base.join("removed")).unwrap();
    rename("keep", "kept");
    rename("source", "empty");
    let still = open("still", OpenMode::Read);

    let dirs = [
        base.join("new/inner"),
        base.join("new"),
        base.join("kept"),
        base.join("empty"),
        base.clone(),
        root.to_path_buf(),
    ];
    let mut items = vec![
        SyncItem::Data(&log),
        SyncItem::Data(&holed),
        SyncItem::All(&grown),
        SyncItem::Data(&old),
        SyncItem::Data(&over),
        SyncItem::Data(&inner),
        SyncItem::Data(&replacement),
    ];
    items.extend(dirs.iter().map(|dir| SyncItem::Dir(dir)));

    let by_device = |file: &File| Ok(Some(file.metadata()?.dev()));
    let no_flush = |_: &File| Ok(());
    let sharing = |flush| SetSync {
        alone: &sync_alone,
        shared: Some(SharedFlush {
            file_system: &by_device,
            hand_over: &hand_to_device,
            flush,
        }),
    };
    let alone = SetSync {
        alone: &sync_alone,
        shared: None,
    };
    match way {
        "together" => vfs.sync_together(&items).unwrap(),
        "together-file-last" => {
            items.reverse();
            vfs.sync_together(&items).unwrap();
        }
        "together-read-last" => {
            items.push(SyncItem::Data(&still));
            vfs.sync_together(&items).unwrap();
        }
        "alone" => sync_together_using(&items, &alone).unwrap(),
        "shared" => sync_together_using(&items, &sharing(&full_flush)).unwrap(),
        "handed" => sync_together_using(&items, &sharing(&no_flush)).unwrap(),
        "none" => {}
        other => panic!("{other} is none of {WAYS:?}"),
    }

    let image = env::var_os("LIGNIN_IO_PROBE_IMAGE").expect("the image file");
    let copy = env::var_os("LIGNIN_IO_PROBE_COPY").expect("where its copy goes");
    let copied = Command::new("cp").arg("-c").args([image, copy]).status();
    assert!(copied.unwrap().success());
}

/// Says what of the changes is in `root`, and returns how many are not as
/// they were made, and of how many.
fn check(root: &Path) -> (usize, usize) {
    let base = root.join("base");
    let log = [pattern(4096, 1), pattern(4096, 2), pattern(4096, 3)].concat();
    let mut holed = vec![0; 1 << 20];
    holed.extend(pattern(4096, 4));
    let mut grown = pattern(100, 5);
    grown.resize(3 << 20, 0);
    let mut over = pattern(8192, 12);
    over[2048..6144].copy_from_slice(&pattern(4096, 6));
    let files = [
        ("log", "appended three times", log.clone()),
        ("holed", "written 1 MiB past its start", holed),
        ("grown", "written and grown to 3 MiB", grown),
        ("old", "cut to 3 bytes", pattern(3, 10)),
        ("kept/over", "overwritten, in the renamed directory", over),
        ("new/inner/f", "made in a new directory", pattern(100, 7)),
        (
            "new/moved",
            "renamed into a new directory",
            pattern(8192, 13),
        ),
        ("new/link", "linked to the appended file", log),
        ("replaced", "replaced by a rename", pattern(5000, 8)),
        (
            "empty/in",
            "in the directory renamed over an empty one",
            pattern(8192, 15),
        ),
        ("still", "not changed", pattern(8192, 16)),
    ];
    let gone = [
        ("gone", "the file, removed"),
        ("removed", "the directory, removed"),
        ("keep", "the directory, renamed away"),
        ("source", "the directory, renamed over another"),
        ("replaced.new", "the file, renamed over another"),
    ];
    let checked = files.len() + gone.len();
    let mut wrong = 0;
    for (path, what, expected) in files {
        let found = match fs::read(base.join(path)) {
            Ok(found) if found == expected => Ok(()),
            Ok(found) => {
                let same = found.iter().zip(&expected).take_while(|(a, b)| a == b);
                Err(format!(
                    "{} bytes of {}, the same up to byte {}",
                    found.len(),
                    expected.len(),
                    same.count()
                ))
            }
            Err(error) => Err(format!("not there ({})", error.kind())),
        };
        wrong += usize::from(found.is_err());
        let found = found.map_or_else(|wrong| wrong, |()| "as it was made".to_owned());
        println!("probe: {path} ({what}): {found}");
    }
    for (path, what) in gone {
        let there = fs::symlink_metadata(base.join(path)).is_ok();
        wrong += usize::from(there);
        let found = if there { "still there" } else { "gone" };
        println!("probe: {path} ({what}): {found}");
    }
    (wrong, checked)
}

/// One step of the experiment: `LIGNIN_IO_PROBE` says which, in the
/// directory `LIGNIN_IO_PROBE_DIR`.
#[test]
#[ignore = "a step of the experiment that device_probe.sh makes on a disk image"]
fn probe() {
    let step = env::var("LIGNIN_IO_PROBE").expect("the step: base, check, or a way to sync");
    let root = PathBuf::from(env::var_os("LIGNIN_IO_PROBE_DIR").expect("the directory"));
    match step.as_str() {
        "base" => base(&root),
        "check" => {
            let (wrong, checked) = check(&root);
            println!("probe: {wrong} of {checked} are not as they were made");
            if env::var_os("LIGNIN_IO_PROBE_WHOLE").is_some() {
                assert_eq!(wrong, 0, "a synced set is not whole on the device");
            }
        }
        way => change(&root, way),
    }
}
