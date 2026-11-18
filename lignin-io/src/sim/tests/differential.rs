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

fn pick<'a>(rng: &mut SimRng, from: &[&'a str]) -> &'a str {
    from[rng.below(from.len() as u64) as usize]
}

#[test]
fn random_operations_end_as_they_do_on_the_operating_system() {
    for seed in 0..40 {
        let in_memory = tempfile::tempdir_in("/dev/shm");
        let dir = in_memory.or_else(|_| tempfile::tempdir()).unwrap();
        let (real, real_root) = (StdVfs, dir.path());
        let (fs, sim_root) = (sim(seed), p("/"));
        let mut rng = SimRng::new(seed);
        let mut handles: Vec<(Box<dyn VfsFile>, Box<dyn VfsFile>)> = Vec::new();
        let mut locks = BTreeMap::new();

        for step in 0..400 {
            let what = format!("seed {seed}, step {step}");
            let anything = [&FILES[..], &DIRS[..]].concat();
            let on = |path: &str| (sim_root.join(path), real_root.join(path));
            match rng.below(26) {
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
                5 | 6 => {
                    let (from, to) = (pick(&mut rng, &FILES), pick(&mut rng, &anything));
                    if from.starts_with(&format!("{to}/")) {
                        // A file renamed onto a directory above it: each
                        // system has its own error for it.
                        continue;
                    }
                    let ((sim_from, real_from), (sim_to, real_to)) = (on(from), on(to));
                    same(
                        fs.rename(&sim_from, &sim_to),
                        real.rename(&real_from, &real_to),
                        &format!("{what}: rename {from} {to}"),
                    );
                }
                7 => {
                    let (original, link) = (pick(&mut rng, &FILES), pick(&mut rng, &anything));
                    let ((sim_from, real_from), (sim_to, real_to)) = (on(original), on(link));
                    same(
                        fs.hard_link(&sim_from, &sim_to),
                        real.hard_link(&real_from, &real_to),
                        &format!("{what}: hard_link {original} {link}"),
                    );
                }
                8 => {
                    let path = pick(&mut rng, &FILES);
                    let (sim_path, real_path) = on(path);
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
}
