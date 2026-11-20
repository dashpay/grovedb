//! The same random operations on the simulator and on the operating
//! system's files: every call ends the same way on both, and so do the
//! trees.

#![cfg(all(not(miri), any(target_os = "linux", target_os = "macos")))]

use super::*;
use crate::StdVfs;

const FILES: [&str; 7] = ["a", "b", "d/a", "d/b", "d/e/a", "a/x", "missing/a"];
const DIRS: [&str; 4] = ["d", "e", "d/e", "missing/dir"];
const LOCKS: [&str; 2] = ["LOCK", "d/LOCK"];

#[track_caller]
fn both<A: Debug, B: Debug>(sim: io::Result<A>, real: io::Result<B>, what: &str) -> Option<(A, B)> {
    match (sim, real) {
        (Ok(sim), Ok(real)) => Some((sim, real)),
        (Err(sim), Err(real)) => {
            assert_eq!(sim.kind(), real.kind(), "{what}: {sim} / {real}");
            None
        }
        (sim, real) => panic!("{what}: the simulator gave {sim:?}, the system {real:?}"),
    }
}

#[track_caller]
fn same<T: Debug + PartialEq>(sim: io::Result<T>, real: io::Result<T>, what: &str) {
    if let Some((sim, real)) = both(sim, real, what) {
        assert_eq!(sim, real, "{what}");
    }
}

/// Compares an operation on two paths. When each path is wrong on its own,
/// the contract does not say which error comes first, and the systems differ:
/// either error is right.
#[track_caller]
fn same_for_two_paths(
    sim: io::Result<()>,
    real: io::Result<()>,
    wrong: [Option<ErrorKind>; 2],
    what: &str,
) {
    if let (Err(sim), Err(real), [Some(first), Some(second)]) = (&sim, &real, wrong) {
        let either = [first, second];
        assert!(either.contains(&sim.kind()), "{what}: {sim}");
        assert!(either.contains(&real.kind()), "{what}: {real}");
        return;
    }
    same(sim, real, what);
}

/// What is wrong with `path` as the second path of an operation: its
/// directory, or the file or the directory that has the name already.
fn wrong_place(
    fs: &SimVfs,
    path: &Path,
    file_there: Option<ErrorKind>,
    dir_there: Option<ErrorKind>,
) -> Option<ErrorKind> {
    let in_dir = fs.read_dir(path.parent().unwrap()).err();
    let there = match fs.open(path, OpenMode::Read) {
        Ok(_) => file_there,
        Err(error) if error.kind() == ErrorKind::IsADirectory => dir_there,
        Err(_) => None,
    };
    in_dir.map(|error| error.kind()).or(there)
}

/// What is wrong with `path` as the first path of an operation: it names
/// nothing, or a directory where that is wrong.
fn not_there(fs: &SimVfs, path: &Path, dir_there: Option<ErrorKind>) -> Option<ErrorKind> {
    match fs.open(path, OpenMode::Read) {
        Ok(_) => None,
        Err(error) if error.kind() == ErrorKind::IsADirectory => dir_there,
        Err(error) => Some(error.kind()),
    }
}

fn pick<'a>(rng: &mut SimRng, from: &[&'a str]) -> &'a str {
    from[rng.below(from.len() as u64) as usize]
}

#[test]
fn random_operations_end_as_they_do_on_the_operating_system() {
    let mut renamed_dirs = 0;
    for seed in 0..40 {
        let dir = tempfile::tempdir().unwrap();
        let (real, real_root) = (StdVfs, dir.path());
        let (fs, sim_root) = (sim(seed), p("/"));
        let mut rng = SimRng::new(seed);
        let mut handles: Vec<(Box<dyn VfsFile>, Box<dyn VfsFile>)> = Vec::new();
        let mut locks = BTreeMap::new();
        let anything = [&FILES[..], &DIRS[..]].concat();

        for step in 0..400 {
            let what = format!("seed {seed}, step {step}");
            let on = |path: &str| (sim_root.join(path), real_root.join(path));
            match rng.below(29) {
                0..=4 => {
                    let (sim_path, real_path) = on(pick(&mut rng, &anything));
                    let create = [Create::Never, Create::IfMissing, Create::New];
                    let create = create[rng.below(3) as usize];
                    let mode = match rng.below(3) {
                        0 => OpenMode::Read,
                        1 => OpenMode::Write(create),
                        _ => OpenMode::Append(create),
                    };
                    let opened = both(
                        fs.open(&sim_path, mode),
                        real.open(&real_path, mode),
                        &format!("{what}: open {sim_path:?} {mode:?}"),
                    );
                    handles.extend(opened);
                }
                5 | 6 | 26..=28 => {
                    let (from, to) = (pick(&mut rng, &anything), pick(&mut rng, &anything));
                    let ((sim_from, real_from), (sim_to, real_to)) = (on(from), on(to));
                    let is_dir = fs.read_dir(&sim_from).is_ok();
                    if !is_dir && from.starts_with(&format!("{to}/")) {
                        // A file renamed onto a directory above it: each
                        // system has its own error for it.
                        continue;
                    }
                    if is_dir && to.starts_with(&format!("{from}/")) {
                        // A directory renamed into itself, where something
                        // may be in the way as well: each system has its
                        // own order of the errors.
                        let either = [
                            ErrorKind::InvalidInput,
                            ErrorKind::NotADirectory,
                            ErrorKind::DirectoryNotEmpty,
                            ErrorKind::NotFound,
                        ];
                        let sim = fs.rename(&sim_from, &sim_to).unwrap_err();
                        let real = real.rename(&real_from, &real_to).unwrap_err();
                        assert!(either.contains(&sim.kind()), "{what}: {sim}");
                        assert!(either.contains(&real.kind()), "{what}: {real}");
                        continue;
                    }
                    let (file_there, dir_there) = match is_dir {
                        true => (Some(ErrorKind::NotADirectory), None),
                        false => (None, Some(ErrorKind::IsADirectory)),
                    };
                    let wrong = [
                        not_there(&fs, &sim_from, None),
                        wrong_place(&fs, &sim_to, file_there, dir_there),
                    ];
                    let renamed = fs.rename(&sim_from, &sim_to);
                    renamed_dirs += u32::from(is_dir && renamed.is_ok() && from != to);
                    same_for_two_paths(
                        renamed,
                        real.rename(&real_from, &real_to),
                        wrong,
                        &format!("{what}: rename {from} {to}"),
                    );
                }
                7 => {
                    let (original, link) = (pick(&mut rng, &FILES), pick(&mut rng, &anything));
                    let ((sim_from, real_from), (sim_to, real_to)) = (on(original), on(link));
                    let taken = Some(ErrorKind::AlreadyExists);
                    let wrong = [
                        not_there(&fs, &sim_from, Some(ErrorKind::PermissionDenied)),
                        wrong_place(&fs, &sim_to, taken, taken),
                    ];
                    same_for_two_paths(
                        fs.hard_link(&sim_from, &sim_to),
                        real.hard_link(&real_from, &real_to),
                        wrong,
                        &format!("{what}: hard_link {original} {link}"),
                    );
                }
                8 => {
                    let path = pick(&mut rng, &FILES);
                    let (sim_path, real_path) = on(path);
                    if fs.read_dir(&sim_path).is_ok() {
                        // A directory where a file is removed: the contract
                        // leaves the error to the system, and Linux and
                        // macOS give different ones.
                        let either = [ErrorKind::IsADirectory, ErrorKind::PermissionDenied];
                        let sim = fs.remove_file(&sim_path).unwrap_err();
                        let real = real.remove_file(&real_path).unwrap_err();
                        assert!(either.contains(&sim.kind()), "{what}: {sim}");
                        assert!(either.contains(&real.kind()), "{what}: {real}");
                        continue;
                    }
                    same(
                        fs.remove_file(&sim_path),
                        real.remove_file(&real_path),
                        &format!("{what}: remove_file {path}"),
                    );
                }
                9 | 10 => {
                    let path = pick(&mut rng, &DIRS);
                    let (sim_path, real_path) = on(path);
                    same(
                        fs.create_dir(&sim_path),
                        real.create_dir(&real_path),
                        &format!("{what}: create_dir {path}"),
                    );
                }
                11 => {
                    let path = pick(&mut rng, &DIRS);
                    let (sim_path, real_path) = on(path);
                    same(
                        fs.remove_dir(&sim_path),
                        real.remove_dir(&real_path),
                        &format!("{what}: remove_dir {path}"),
                    );
                }
                12 => {
                    let path = pick(&mut rng, &anything);
                    let (sim_path, real_path) = on(path);
                    same(
                        fs.read_dir(&sim_path),
                        real.read_dir(&real_path),
                        &format!("{what}: read_dir {path}"),
                    );
                    same(
                        fs.sync_dir(&sim_path),
                        real.sync_dir(&real_path),
                        &format!("{what}: sync_dir {path}"),
                    );
                }
                13 => {
                    let path = pick(&mut rng, &LOCKS);
                    if locks.remove(path).is_none() {
                        let (sim_path, real_path) = on(path);
                        let locked = both(
                            fs.lock(&sim_path),
                            real.lock(&real_path),
                            &format!("{what}: lock {path}"),
                        );
                        locks.extend(locked.map(|guards| (path, guards)));
                        same(
                            fs.lock(&sim_path).map(drop),
                            real.lock(&real_path).map(drop),
                            &format!("{what}: lock {path} again"),
                        );
                    }
                }
                14 if !handles.is_empty() => {
                    handles.swap_remove(rng.below(handles.len() as u64) as usize);
                }
                _ if !handles.is_empty() => {
                    let (sim_file, real_file) = &handles[rng.below(handles.len() as u64) as usize];
                    let offset = rng.below(90);
                    let data: Vec<u8> = (0..rng.below(40)).map(|_| rng.next_u64() as u8).collect();
                    match rng.below(9) {
                        0 | 1 => {
                            let (mut sim_buf, mut real_buf) = (vec![0; 50], vec![0; 50]);
                            same(
                                sim_file.read_at(&mut sim_buf, offset),
                                real_file.read_at(&mut real_buf, offset),
                                &format!("{what}: read_at {offset}"),
                            );
                            assert_eq!(sim_buf, real_buf, "{what}");
                            same(
                                sim_file.read_exact_at(&mut sim_buf[..9], offset),
                                real_file.read_exact_at(&mut real_buf[..9], offset),
                                &format!("{what}: read_exact_at {offset}"),
                            );
                        }
                        2 | 3 => same(
                            sim_file.write_at(&data, offset),
                            real_file.write_at(&data, offset),
                            &format!("{what}: write_at {offset}"),
                        ),
                        4 | 5 => same(
                            sim_file.append(&data),
                            real_file.append(&data),
                            &format!("{what}: append"),
                        ),
                        6 => same(
                            sim_file.set_len(offset),
                            real_file.set_len(offset),
                            &format!("{what}: set_len {offset}"),
                        ),
                        7 => same(sim_file.len(), real_file.len(), &format!("{what}: len")),
                        _ => {
                            same(sim_file.sync_data(), real_file.sync_data(), &what);
                            same(sim_file.sync_all(), real_file.sync_all(), &what);
                        }
                    }
                }
                _ => {}
            }
            check(&fs);

            if step % 40 == 39 {
                assert_eq!(tree(&fs), tree_under(&real, real_root), "{what}");
                let image = fs.crash_image();
                let kept = recover(&image, &image.keep_all());
                assert_eq!(kept.fingerprint(), fs.fingerprint(), "{what}");
            }
        }
        assert_eq!(fs.stats().faults, 0);
    }
    assert!(renamed_dirs > 100, "{renamed_dirs} directories renamed");
}
