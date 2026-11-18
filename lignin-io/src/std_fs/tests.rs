use std::{
    env,
    io::{BufRead, BufReader},
    path::PathBuf,
    process::{Child, Command, Stdio},
};

use tempfile::TempDir;

use super::*;

fn setup() -> (StdVfs, PathBuf, TempDir) {
    let dir = tempfile::tempdir().unwrap();
    (StdVfs, dir.path().to_path_buf(), dir)
}

mod conformance {
    use super::setup;

    crate::conformance::conformance_tests!(setup());
}

fn append_file(path: &Path) -> StdFile {
    StdVfs::open_file(path, OpenMode::Append(Create::New)).unwrap()
}

#[test]
fn append_is_continued_after_short_and_interrupted_writes() {
    let (_, root, _dir) = setup();
    let path = root.join("file");
    let file = append_file(&path);
    file.append(b"start ").unwrap();

    let mut calls = 0;
    let offset = file
        .append_using(b"in three pieces", &mut |mut file, bytes| {
            calls += 1;
            if calls % 2 == 0 {
                return Err(ErrorKind::Interrupted.into());
            }
            file.write(&bytes[..bytes.len().min(6)])
        })
        .unwrap();

    assert_eq!(offset, 6);
    assert!(calls >= 5);
    assert_eq!(file.append(b"!").unwrap(), 21);
    assert_eq!(fs::read(&path).unwrap(), b"start in three pieces!");
}

#[test]
fn a_failed_append_is_undone() {
    let (_, root, _dir) = setup();
    let path = root.join("file");
    let file = append_file(&path);
    file.append(b"committed").unwrap();

    let mut calls = 0;
    let error = file
        .append_using(&[7; 100], &mut |mut file, bytes| {
            calls += 1;
            if calls == 1 {
                file.write(&bytes[..40])
            } else {
                Err(ErrorKind::StorageFull.into())
            }
        })
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::StorageFull);
    assert_eq!(fs::read(&path).unwrap(), b"committed");

    let error = file.append_using(&[7; 100], &mut |_, _| Ok(0)).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::WriteZero);

    assert_eq!(file.append(b" next").unwrap(), 9);
    assert_eq!(fs::read(&path).unwrap(), b"committed next");
}

#[test]
fn an_append_that_cannot_be_undone_breaks_the_handle() {
    let (_, root, _dir) = setup();
    let path = root.join("file");
    let file = append_file(&path);
    file.append(b"committed").unwrap();

    let mut torn = None;
    let error = file
        .append_using(&[7; 100], &mut |mut file, bytes| {
            if torn.is_some() {
                return Err(ErrorKind::StorageFull.into());
            }
            let mut other = OpenOptions::new().append(true).open(&path)?;
            other.write_all(b" foreign ")?;
            let written = file.write(&bytes[..40])?;
            torn = Some(written);
            Ok(written)
        })
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::StorageFull);

    let mut untouched = b"committed foreign ".to_vec();
    untouched.extend_from_slice(&[7; 100][..torn.unwrap()]);
    assert_eq!(fs::read(&path).unwrap(), untouched);

    for _ in 0..2 {
        let error = file.append(b"x").unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Other);
        assert!(error.to_string().contains("could not be undone"));
    }
    assert_eq!(fs::read(&path).unwrap(), untouched);
    assert_eq!(file.len().unwrap(), untouched.len() as u64);
    file.sync_data().unwrap();
}

#[test]
fn append_never_overwrites_what_another_handle_appended() {
    let (_, root, _dir) = setup();
    let path = root.join("file");
    let file = append_file(&path);
    file.append(b"first ").unwrap();

    let other = StdVfs.open(&path, OpenMode::Append(Create::Never)).unwrap();
    other.append(b"behind its back ").unwrap();
    file.append(b"last").unwrap();

    assert_eq!(fs::read(&path).unwrap(), b"first behind its back last");
}

#[test]
fn a_failed_sync_sticks() {
    let (_, root, _dir) = setup();
    let path = root.join("file");
    let file = StdVfs::open_file(&path, OpenMode::Write(Create::New)).unwrap();
    file.write_at(b"content", 0).unwrap();
    file.sync_data().unwrap();

    let error = file
        .sync_using(|_| Err(ErrorKind::StorageFull.into()))
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::StorageFull);

    for sync in [StdFile::sync_data, StdFile::sync_all] {
        let error = sync(&file).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::StorageFull);
        assert!(error.to_string().contains("an earlier sync"));
    }

    file.write_at(b"CONTENT", 0).unwrap();
    assert_eq!(fs::read(&path).unwrap(), b"CONTENT");
    StdVfs
        .open(&path, OpenMode::Read)
        .unwrap()
        .sync_all()
        .unwrap();
}

#[test]
#[cfg_attr(miri, ignore = "Miri has no hard links")]
fn hard_link_shares_the_inode() {
    let (vfs, root, _dir) = setup();
    let (original, link) = (root.join("original"), root.join("link"));
    fs::write(&original, b"content").unwrap();
    vfs.hard_link(&original, &link).unwrap();

    let (original, link) = (
        fs::metadata(&original).unwrap(),
        fs::metadata(&link).unwrap(),
    );
    assert_eq!((original.dev(), original.ino()), (link.dev(), link.ino()));
    assert_eq!(original.nlink(), 2);
}

#[test]
fn read_dir_does_not_follow_symbolic_links() {
    let (vfs, root, _dir) = setup();
    fs::create_dir(root.join("dir")).unwrap();
    std::os::unix::fs::symlink(root.join("dir"), root.join("link")).unwrap();

    let entries = vfs.read_dir(&root).unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].kind, EntryKind::Dir);
    assert_eq!(entries[1].kind, EntryKind::Other);
}

#[test]
#[cfg_attr(miri, ignore = "Miri has no inode numbers")]
fn a_lock_knows_whether_its_file_still_has_its_name() {
    let (_, root, _dir) = setup();
    let path = root.join("LOCK");
    let lock = StdLock {
        file: File::create(&path).unwrap(),
    };
    assert!(lock.is_named(&path).unwrap());

    fs::rename(&path, root.join("moved")).unwrap();
    assert!(!lock.is_named(&path).unwrap());
    File::create(&path).unwrap();
    assert!(!lock.is_named(&path).unwrap());
    assert!(lock.is_named(&root.join("moved")).unwrap());
}

#[cfg(target_os = "linux")]
#[test]
#[cfg_attr(miri, ignore = "needs /dev/full")]
fn append_to_a_full_device_is_storage_full() {
    let file = StdVfs
        .open(Path::new("/dev/full"), OpenMode::Append(Create::Never))
        .unwrap();
    for _ in 0..2 {
        assert_eq!(
            file.append(b"x").unwrap_err().kind(),
            ErrorKind::StorageFull
        );
    }
}

#[cfg(target_os = "linux")]
#[test]
#[cfg_attr(miri, ignore = "needs a second filesystem")]
fn hard_link_to_another_filesystem_crosses_devices() {
    let (vfs, root, _dir) = setup();
    let Ok(other) = tempfile::tempdir_in("/dev/shm") else {
        eprintln!("skipped: /dev/shm is not usable");
        return;
    };
    if fs::metadata(&root).unwrap().dev() == fs::metadata(other.path()).unwrap().dev() {
        eprintln!("skipped: /dev/shm is on the filesystem of the temporary directory");
        return;
    }
    fs::write(root.join("file"), b"content").unwrap();

    let error = vfs
        .hard_link(&root.join("file"), &other.path().join("link"))
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::CrossesDevices);
    let error = vfs
        .rename(&root.join("file"), &other.path().join("moved"))
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::CrossesDevices);
    assert_eq!(fs::read(root.join("file")).unwrap(), b"content");
}

const ROLE: &str = "LIGNIN_IO_TEST_ROLE";
const LOCK_PATH: &str = "LIGNIN_IO_TEST_LOCK_PATH";
const CHILD_SAYS: &str = "lock child: ";

fn lock_child(role: &str, path: &Path) -> Child {
    let test = concat!(module_path!(), "::lock_excludes_other_processes");
    let test = test.split_once("::").unwrap().1;
    Command::new(env::current_exe().unwrap())
        .args(["--exact", test, "--nocapture", "--test-threads=1"])
        .env(ROLE, role)
        .env(LOCK_PATH, path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap()
}

fn child_report(child: &mut Child) -> String {
    let stdout = BufReader::new(child.stdout.as_mut().unwrap());
    stdout
        .lines()
        .map(Result::unwrap)
        .find_map(|line| Some(line.split_once(CHILD_SAYS)?.1.to_owned()))
        .expect("the child process reports before it ends")
}

fn try_lock_in_child(path: &Path) -> String {
    let mut child = lock_child("try", path);
    let report = child_report(&mut child);
    assert!(child.wait().unwrap().success());
    report
}

#[test]
#[cfg_attr(miri, ignore = "spawns processes")]
fn lock_excludes_other_processes() {
    if let Ok(role) = env::var(ROLE) {
        let path = PathBuf::from(env::var_os(LOCK_PATH).unwrap());
        let attempt = StdVfs.lock(&path);
        match (role.as_str(), &attempt) {
            ("try", Ok(_)) => println!("{CHILD_SAYS}locked"),
            ("try", Err(error)) => println!("{CHILD_SAYS}{:?}", error.kind()),
            ("hold", Ok(_)) => {
                println!("{CHILD_SAYS}locked");
                io::stdin().read_line(&mut String::new()).unwrap();
            }
            _ => panic!("{role}: {attempt:?}"),
        }
        return;
    }

    let (vfs, root, _dir) = setup();
    let path = root.join("LOCK");

    let guard = vfs.lock(&path).unwrap();
    assert_eq!(try_lock_in_child(&path), "WouldBlock");
    drop(guard);
    assert_eq!(try_lock_in_child(&path), "locked");

    let mut holder = lock_child("hold", &path);
    assert_eq!(child_report(&mut holder), "locked");
    assert_eq!(vfs.lock(&path).unwrap_err().kind(), ErrorKind::WouldBlock);
    holder.kill().unwrap();
    holder.wait().unwrap();
    drop(vfs.lock(&path).unwrap());
}
