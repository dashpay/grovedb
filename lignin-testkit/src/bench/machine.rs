//! What the harness learns from the machine it runs on: its description,
//! the commit, CPU counters beside wall time, and how to make a read cold.
//!
//! Everything here goes through files and commands, so the crate stays
//! free of unsafe code. What a machine does not offer comes back as `None`
//! and is reported as missing, not guessed.

use std::{
    fs::{File, OpenOptions},
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::mpsc,
    time::Duration,
};

use serde::{Deserialize, Serialize};

/// The machine a run was made on.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Machine {
    /// A label given on the command line, such as `strixbox`.
    pub label: Option<String>,
    /// `std::env::consts::OS`.
    pub os: String,
    /// `std::env::consts::ARCH`.
    pub arch: String,
    /// The processor's name.
    pub cpu: Option<String>,
    /// Logical processors available to the process.
    pub logical_cpus: usize,
    /// Memory the system reports (in a container, its limit).
    pub memory_bytes: Option<u64>,
}

impl Machine {
    /// Describes this machine.
    pub fn detect(label: Option<String>) -> Self {
        Machine {
            label,
            os: std::env::consts::OS.to_string(),
            arch: std::env::consts::ARCH.to_string(),
            cpu: cpu_name(),
            logical_cpus: std::thread::available_parallelism().map_or(0, usize::from),
            memory_bytes: memory_bytes(),
        }
    }
}

/// The processor's name: on Linux the first non-empty `model name` of
/// `/proc/cpuinfo`. An aarch64 kernel writes no such line for a 64-bit
/// process, only the processor's codes, so there it is `lscpu`'s model name,
/// which knows the names of the codes, or the codes themselves.
fn cpu_name() -> Option<String> {
    let Ok(cpuinfo) = std::fs::read_to_string("/proc/cpuinfo") else {
        return command_output("sysctl", &["-n", "machdep.cpu.brand_string"]);
    };
    field(&cpuinfo, "model name")
        .map(str::to_string)
        .or_else(|| command_output("lscpu", &[]).and_then(|text| lscpu_model_name(&text)))
        .or_else(|| arm_cpu_name(&cpuinfo))
}

/// The first non-empty value of a line `key: value` of `text`.
fn field<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    text.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        let value = value.trim();
        (name.trim() == key && !value.is_empty()).then_some(value)
    })
}

/// `lscpu` writes `-` where it does not know the model.
fn lscpu_model_name(text: &str) -> Option<String> {
    field(text, "Model name")
        .filter(|name| *name != "-")
        .map(str::to_string)
}

/// The maker and part of an Arm processor, from the `CPU implementer` and
/// `CPU part` lines of an aarch64 `/proc/cpuinfo` (written by `c_show` in the
/// kernel's `arch/arm64/kernel/cpuinfo.c`), such as `Arm part 0xd0c` for a
/// Neoverse N1.
fn arm_cpu_name(cpuinfo: &str) -> Option<String> {
    let implementer = field(cpuinfo, "CPU implementer")?;
    let part = field(cpuinfo, "CPU part")?;
    let code = u8::from_str_radix(implementer.strip_prefix("0x")?, 16).ok()?;
    let maker = match code {
        0x41 => "Arm",
        0x42 => "Broadcom",
        0x46 => "Fujitsu",
        0x48 => "HiSilicon",
        0x4e => "NVIDIA",
        0x51 => "Qualcomm",
        0x61 => "Apple",
        0xc0 => "Ampere",
        _ => return Some(format!("implementer {implementer} part {part}")),
    };
    Some(format!("{maker} part {part}"))
}

fn memory_bytes() -> Option<u64> {
    if let Ok(meminfo) = std::fs::read_to_string("/proc/meminfo") {
        return meminfo
            .lines()
            .find_map(|line| line.strip_prefix("MemTotal:"))
            .and_then(|rest| rest.trim().strip_suffix("kB"))
            .and_then(|kib| kib.trim().parse::<u64>().ok())
            .map(|kib| kib * 1024);
    }
    command_output("sysctl", &["-n", "hw.memsize"]).and_then(|bytes| bytes.parse().ok())
}

/// The system's load averages over 1, 5 and 15 minutes.
pub fn load_average() -> Option<[f64; 3]> {
    let text = std::fs::read_to_string("/proc/loadavg")
        .ok()
        .or_else(|| command_output("sysctl", &["-n", "vm.loadavg"]))?;
    parse_load_average(&text)
}

fn parse_load_average(text: &str) -> Option<[f64; 3]> {
    let mut numbers = text
        .split(|c: char| c.is_whitespace() || c == '{' || c == '}')
        .filter(|word| !word.is_empty())
        .map(|word| word.parse::<f64>());
    let mut next = || numbers.next()?.ok();
    Some([next()?, next()?, next()?])
}

/// The commit of the source tree at `dir`, with `-dirty` appended when it
/// has uncommitted changes.
pub fn source_commit(dir: &Path) -> Option<String> {
    let dir = dir.to_str()?;
    let head = command_output("git", &["-C", dir, "rev-parse", "HEAD"])?;
    let dirty = Command::new("git")
        .args(["-C", dir, "status", "--porcelain", "--untracked-files=no"])
        .output()
        .ok()
        .is_some_and(|output| !output.stdout.is_empty());
    Some(if dirty { format!("{head}-dirty") } else { head })
}

/// What a command prints, in the C locale, whose labels are the ones parsed
/// here (`lscpu` translates its own).
fn command_output(program: &str, args: &[&str]) -> Option<String> {
    let output = Command::new(program)
        .args(args)
        .env("LC_ALL", "C")
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?.trim().to_string();
    (!text.is_empty()).then_some(text)
}

/// Nanoseconds the calling thread has spent on a CPU, from
/// `/proc/thread-self/schedstat` (Linux only).
pub fn thread_cpu_ns() -> Option<u64> {
    let text = std::fs::read_to_string("/proc/thread-self/schedstat").ok()?;
    text.split_whitespace().next()?.parse().ok()
}

/// The calling thread's id, from `/proc/thread-self` (Linux only).
pub fn thread_id() -> Option<u32> {
    let link = std::fs::read_link("/proc/thread-self").ok()?;
    link.file_name()?.to_str()?.parse().ok()
}

/// A counter of `/proc/self/io` (Linux only), such as `read_bytes`: bytes
/// the process caused to be read from storage.
pub fn process_io(field: &str) -> Option<u64> {
    let text = std::fs::read_to_string("/proc/self/io").ok()?;
    text.lines()
        .find_map(|line| line.strip_prefix(field)?.strip_prefix(':'))
        .and_then(|value| value.trim().parse().ok())
}

/// What the operating system has counted of the process's reads at one
/// moment (Linux only). The harness reads it before and after a phase or
/// a kind of read and subtracts ([`StorageReads::since`]).
///
/// A count is `None` where the system does not give it. The two counts of
/// the cgroup need cgroup v2 with the `io` controller on for the cgroup
/// the process runs in, which a systemd unit with `IOAccounting=yes` has.
/// They are of every process of that cgroup. Under a memory limit on the
/// cgroup that leaves the kernel's page cache little room, they say how
/// often a read reached the device; with the page cache free, a page that
/// was read or written before is read from memory and counted nowhere
/// here but in `process_calls`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StorageReads {
    /// `/proc/self/io` `read_bytes`: bytes the process caused to be read
    /// from storage, read-ahead included.
    pub process_bytes: Option<u64>,
    /// `/proc/self/io` `syscr`: the process's read system calls, whatever
    /// they read and wherever it came from.
    pub process_calls: Option<u64>,
    /// `io.stat` `rios` of the process's cgroup, summed over its devices:
    /// read requests that reached a device.
    pub device_reads: Option<u64>,
    /// `io.stat` `rbytes`, summed the same way: the bytes of those
    /// requests.
    pub device_bytes: Option<u64>,
}

impl StorageReads {
    /// The counts now.
    pub fn now() -> StorageReads {
        let device = cgroup_io_stat().map(|text| device_reads(&text));
        StorageReads {
            process_bytes: process_io("read_bytes"),
            process_calls: process_io("syscr"),
            device_reads: device.map(|(reads, _)| reads),
            device_bytes: device.map(|(_, bytes)| bytes),
        }
    }

    /// What was counted between `earlier` and `self`. A count that either
    /// reading lacks is `None`.
    pub fn since(&self, earlier: &StorageReads) -> StorageReads {
        let since = |now: Option<u64>, then: Option<u64>| {
            now.zip(then).map(|(now, then)| now.saturating_sub(then))
        };
        StorageReads {
            process_bytes: since(self.process_bytes, earlier.process_bytes),
            process_calls: since(self.process_calls, earlier.process_calls),
            device_reads: since(self.device_reads, earlier.device_reads),
            device_bytes: since(self.device_bytes, earlier.device_bytes),
        }
    }
}

/// The `io.stat` file of the cgroup this process runs in (cgroup v2), or
/// nothing where the cgroup's directory is not known or the file cannot be
/// read.
fn cgroup_io_stat() -> Option<String> {
    let groups = std::fs::read_to_string("/proc/self/cgroup").ok()?;
    let group = groups.lines().find_map(|line| line.strip_prefix("0::"))?;
    let mounts = std::fs::read_to_string("/proc/self/mountinfo").ok()?;
    std::fs::read_to_string(cgroup_dir(&mounts, group)?.join("io.stat")).ok()
}

/// The directory of the cgroup `group` (its path in `/proc/self/cgroup`)
/// by `mountinfo` (the text of `/proc/self/mountinfo`), in the one layout
/// this reads: the cgroup v2 hierarchy mounted whole, once, at
/// `/sys/fs/cgroup`, with nothing else mounted there or under it. The
/// directory is then that one and the group's path.
///
/// In every other layout there is none, and a run has no counts of its
/// cgroup: a part of the hierarchy mounted there, a second mount over it,
/// a mount of anything under it, the hierarchy mounted elsewhere only, a
/// table this cannot read, or a group whose path climbs (`..`). A path
/// built in one of those could be another group's.
fn cgroup_dir(mountinfo: &str, group: &str) -> Option<PathBuf> {
    const AT: &str = "/sys/fs/cgroup";
    let climbs = Path::new(group)
        .components()
        .any(|part| part == std::path::Component::ParentDir);
    if !group.starts_with('/') || climbs {
        return None;
    }
    let mut found = false;
    for line in mountinfo.lines() {
        // The fields of a line are parted by one space each, and a space
        // in a path is written as an escape.
        let (mount, filesystem) = line.split_once(" - ")?;
        let mut fields = mount.split(' ').skip(3);
        let (mount_root, mount_point) = (fields.next()?, fields.next()?);
        let under = mount_point
            .strip_prefix(AT)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'));
        if !under {
            continue;
        }
        let whole = mount_point == AT
            && mount_root == "/"
            && filesystem.split(' ').next() == Some("cgroup2");
        if !whole || found {
            return None;
        }
        found = true;
    }
    found.then(|| Path::new(AT).join(group.trim_start_matches('/')))
}

/// The read requests and the bytes read of every device in the text of
/// an `io.stat` file, summed. A line is a device's numbers and then
/// `key=value` fields; a field that is not a number counts as nothing.
fn device_reads(io_stat: &str) -> (u64, u64) {
    let sum = |key: &str| {
        io_stat
            .split_whitespace()
            .filter_map(|field| field.strip_prefix(key)?.strip_prefix('='))
            .filter_map(|value| value.parse::<u64>().ok())
            .sum()
    };
    (sum("rios"), sum("rbytes"))
}

/// How a cold read is made cold.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ColdMethod {
    /// The database is closed and opened again, which empties the engine's
    /// own caches. The operating system's page cache keeps the files.
    Reopen,
    /// As [`ColdMethod::Reopen`], and while it is closed every file of the
    /// database is dropped from the page cache with
    /// `posix_fadvise(POSIX_FADV_DONTNEED)` (through Python's
    /// `os.posix_fadvise`, the crate having no unsafe code), which drops a
    /// file's clean pages without privilege, unlike
    /// `/proc/sys/vm/drop_caches`. Chosen only where a probe file was seen
    /// to be read from storage again after the drop.
    ReopenAndDropFileCache,
}

const COLD_PROBE_BYTES: usize = 4 << 20;

impl ColdMethod {
    /// The strongest method this machine allows: dropping files from the
    /// page cache where that is shown to work, reopening only elsewhere.
    pub fn detect(scratch: &Path) -> ColdMethod {
        let probe = scratch.join("cold-probe");
        let works = cache_drop_works(&probe);
        let _ = std::fs::remove_file(&probe);
        if works {
            ColdMethod::ReopenAndDropFileCache
        } else {
            ColdMethod::Reopen
        }
    }
}

/// Whether dropping `probe` from the page cache makes reading it again
/// read storage: a cached read reads less than half the file from
/// storage, a read after the drop more than half.
fn cache_drop_works(probe: &Path) -> bool {
    let written = File::create(probe).and_then(|mut file| {
        // Bytes a compressing file system cannot shrink.
        let mut state = 0x9E37_79B9_7F4A_7C15u64;
        let bytes: Vec<u8> = (0..COLD_PROBE_BYTES)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                state as u8
            })
            .collect();
        file.write_all(&bytes)?;
        file.sync_all()
    });
    let storage_read = || -> Option<u64> {
        let before = process_io("read_bytes")?;
        std::fs::read(probe).ok()?;
        Some(process_io("read_bytes")?.saturating_sub(before))
    };
    let half = COLD_PROBE_BYTES as u64 / 2;
    written.is_ok()
        && storage_read().is_some()
        && storage_read().is_some_and(|cached| cached < half)
        && drop_files_from_cache(&[probe.to_path_buf()])
        && storage_read().is_some_and(|cold| cold > half)
}

const FADVISE_SCRIPT: &str = "import os, sys
for path in sys.argv[1:]:
    try:
        fd = os.open(path, os.O_RDONLY)
    except FileNotFoundError:
        continue
    os.posix_fadvise(fd, 0, 0, os.POSIX_FADV_DONTNEED)
    os.close(fd)
";

/// Drops `files` from the page cache. A file that no longer exists is
/// skipped. Returns false if the drop could not be made.
pub fn drop_files_from_cache(files: &[PathBuf]) -> bool {
    Command::new("python3")
        .args(["-I", "-c", FADVISE_SCRIPT])
        .args(files)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// Every file under `dir`, in its subdirectories too. A file or directory
/// the engine deletes while it is being listed is left out.
pub fn files_under(dir: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    let root = dir;
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound && dir != root => {
                continue;
            }
            Err(error) => return Err(error),
        };
        for entry in entries {
            let entry = entry?;
            match entry.file_type() {
                Ok(kind) if kind.is_dir() => pending.push(entry.path()),
                Ok(kind) if kind.is_file() => files.push(entry.path()),
                _ => {}
            }
        }
    }
    files.sort();
    Ok(files)
}

/// Copies the directory `from`, with every file and every directory under
/// it, to `to`, which must not exist yet. What is neither a file nor a
/// directory is left out. If the copy fails after `to` was made, what was
/// copied is removed again, so that a copy is whole or is not there; where
/// that removal fails too, the error says so, with the path.
pub fn copy_tree(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir(to)?;
    copy_into(from, to).map_err(|error| match std::fs::remove_dir_all(to) {
        Ok(()) => error,
        Err(left) => std::io::Error::new(
            error.kind(),
            format!(
                "{error}; and what was copied to {} could not be removed: {left}",
                to.display()
            ),
        ),
    })
}

/// Copies what is in the directory `from` into the directory `to`.
fn copy_into(from: &Path, to: &Path) -> std::io::Result<()> {
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        let kind = entry.file_type()?;
        if kind.is_dir() {
            std::fs::create_dir(&target)?;
            copy_into(&entry.path(), &target)?;
        } else if kind.is_file() {
            std::fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

/// Drops every file under `dir` from the page cache. Returns false if the
/// directory cannot be listed or the drop could not be made.
pub fn drop_dir_cache(dir: &Path) -> bool {
    files_under(dir).is_ok_and(|files| drop_files_from_cache(&files))
}

/// User-space instructions retired by one thread, counted by `perf stat`
/// attached to it, only while enabled.
///
/// `perf` starts with counting off and is switched through its control
/// FIFO, so only the measured sections count. Counts accumulate over every
/// enabled section until [`Perf::finish`].
pub struct Perf {
    child: Child,
    control: File,
    acks: mpsc::Receiver<String>,
    output: PathBuf,
    dir: PathBuf,
}

const PERF_ACK_TIMEOUT: Duration = Duration::from_secs(10);

impl Perf {
    /// Attaches `perf stat -e instructions:u` to thread `tid`, keeping its
    /// files in a new directory `dir`. Returns `None` where `perf` is
    /// missing or refuses.
    pub fn attach(tid: u32, dir: &Path) -> Option<Perf> {
        std::fs::create_dir_all(dir).ok()?;
        let control_path = dir.join("control");
        let ack_path = dir.join("ack");
        for fifo in [&control_path, &ack_path] {
            let _ = std::fs::remove_file(fifo);
            let made = Command::new("mkfifo").arg(fifo).status().ok()?;
            if !made.success() {
                return None;
            }
        }
        let output = dir.join("perf.csv");
        // Opened read-write so neither open waits for perf to open its end.
        let control = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&control_path)
            .ok()?;
        let ack = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&ack_path)
            .ok()?;
        let child = Command::new("perf")
            .args(["stat", "-x", ",", "-e", "instructions:u", "--delay=-1"])
            .arg(format!(
                "--control=fifo:{},{}",
                control_path.display(),
                ack_path.display()
            ))
            .args(["-t", &tid.to_string(), "-o"])
            .arg(&output)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let (sender, acks) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(ack).lines() {
                let Ok(line) = line else {
                    return;
                };
                if sender.send(line).is_err() {
                    return;
                }
            }
        });
        let mut perf = Perf {
            child,
            control,
            acks,
            output,
            dir: dir.to_path_buf(),
        };
        if perf.command("ping") {
            Some(perf)
        } else {
            perf.abandon();
            None
        }
    }

    /// Whether `perf` counts the calling thread's instructions here.
    pub fn works(scratch: &Path) -> bool {
        let Some(tid) = thread_id() else {
            return false;
        };
        let Some(mut perf) = Perf::attach(tid, &scratch.join("perf-probe")) else {
            return false;
        };
        let switched = perf.enable() && perf.disable();
        perf.finish().is_some() && switched
    }

    fn command(&mut self, command: &str) -> bool {
        if writeln!(self.control, "{command}").is_err() {
            return false;
        }
        // perf writes each "ack\n" with its C string terminator.
        matches!(
            self.acks.recv_timeout(PERF_ACK_TIMEOUT),
            Ok(line) if line.trim_matches(|c: char| c.is_whitespace() || c == '\0') == "ack"
        )
    }

    /// Starts counting.
    pub fn enable(&mut self) -> bool {
        self.command("enable")
    }

    /// Stops counting.
    pub fn disable(&mut self) -> bool {
        self.command("disable")
    }

    /// Stops `perf` and returns the instructions counted while enabled.
    pub fn finish(mut self) -> Option<u64> {
        let stopped = Command::new("kill")
            .args(["-INT", &self.child.id().to_string()])
            .status()
            .is_ok_and(|status| status.success());
        if !stopped {
            self.abandon();
            return None;
        }
        let _ = self.child.wait();
        let text = std::fs::read_to_string(&self.output).ok();
        let _ = std::fs::remove_dir_all(&self.dir);
        parse_perf_csv(&text?)
    }

    fn abandon(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// The count of the `instructions:u` line of `perf stat -x ,` output.
fn parse_perf_csv(text: &str) -> Option<u64> {
    text.lines()
        .filter(|line| line.contains("instructions"))
        .find_map(|line| line.split(',').next()?.trim().parse().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_copy_of_a_directory_is_whole_or_not_there() {
        let dir = tempfile::tempdir().unwrap();
        let from = dir.path().join("from");
        std::fs::create_dir_all(from.join("inner")).unwrap();
        std::fs::write(from.join("a"), b"one").unwrap();
        std::fs::write(from.join("inner").join("b"), b"two").unwrap();

        let to = dir.path().join("to");
        copy_tree(&from, &to).unwrap();
        assert_eq!(std::fs::read(to.join("a")).unwrap(), b"one");
        assert_eq!(std::fs::read(to.join("inner").join("b")).unwrap(), b"two");

        // A copy onto what is there is refused, and what is there stays.
        let refused = copy_tree(&from, &to).unwrap_err();
        assert_eq!(refused.kind(), std::io::ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read(to.join("a")).unwrap(), b"one");

        // A copy that fails after its directory was made leaves none.
        let partial = dir.path().join("partial");
        assert!(copy_tree(&dir.path().join("missing"), &partial).is_err());
        assert!(!partial.exists());
    }

    #[test]
    fn the_directory_of_a_cgroup_is_known_in_one_layout_of_the_mounts() {
        let dir = |mountinfo: &str, group: &str| {
            cgroup_dir(mountinfo, group)
                .map(|dir| dir.to_string_lossy().trim_end_matches('/').to_owned())
        };
        let found = |path: &str| Some(path.to_owned());
        let sys = "24 30 0:22 / /sys rw,nosuid - sysfs sysfs rw\n";
        let whole = "35 24 0:30 / /sys/fs/cgroup rw,nosuid,nodev - cgroup2 cgroup2 rw,nsdelegate\n";
        let usual = format!("{sys}{whole}36 24 0:31 / /sys/fs/bpf rw - bpf bpf rw\n");
        assert_eq!(
            dir(&usual, "/system.slice/run.service"),
            found("/sys/fs/cgroup/system.slice/run.service")
        );
        assert_eq!(dir(&usual, "/"), found("/sys/fs/cgroup"));
        // A path that climbs, or that is none, names no directory.
        assert_eq!(dir(&usual, "/../sibling"), None);
        assert_eq!(dir(&usual, "/a/../../b"), None);
        assert_eq!(dir(&usual, "run"), None);

        // A part of the hierarchy mounted there: its paths are not the
        // group's.
        let part = "35 24 0:30 /team /sys/fs/cgroup rw - cgroup2 cgroup2 rw\n";
        assert_eq!(dir(part, "/team/run"), None);
        // A second mount there covers the first, and a mount under it
        // covers a part of it: whichever is seen is not known.
        assert_eq!(dir(&format!("{part}{whole}"), "/team/run"), None);
        assert_eq!(dir(&format!("{whole}{whole}"), "/run"), None);
        let over = "40 35 0:40 / /sys/fs/cgroup/run rw - tmpfs tmpfs rw\n";
        assert_eq!(dir(&format!("{whole}{over}"), "/run"), None);
        let hybrid = "35 24 0:30 / /sys/fs/cgroup ro - tmpfs tmpfs ro\n\
                      36 35 0:31 / /sys/fs/cgroup/unified rw - cgroup2 cgroup2 rw\n";
        assert_eq!(dir(hybrid, "/run"), None);
        // The hierarchy mounted elsewhere only, or not at all.
        let elsewhere = "35 24 0:30 / /mnt/cg rw - cgroup2 cgroup2 rw\n";
        assert_eq!(dir(elsewhere, "/run"), None);
        assert_eq!(dir(sys, "/run"), None);
        assert_eq!(dir("", "/run"), None);
        // A line that is no line of the table.
        assert_eq!(dir(&format!("{whole}nonsense\n"), "/run"), None);

        // A path beside it whose name only begins alike is another place,
        // also where it holds a space that is no ASCII space, which the
        // table writes as it is.
        let beside = "41 24 0:41 / /sys/fs/cgroup2 rw - cgroup2 cgroup2 rw\n\
                      42 24 0:42 / /sys/fs/cgroup\u{a0}one rw - cgroup2 cgroup2 rw\n";
        assert_eq!(
            dir(&format!("{whole}{beside}"), "/run"),
            found("/sys/fs/cgroup/run")
        );
    }

    #[test]
    fn load_averages_parse_from_both_systems() {
        assert_eq!(
            parse_load_average("0.52 0.58 0.59 1/467 12345\n"),
            Some([0.52, 0.58, 0.59])
        );
        assert_eq!(
            parse_load_average("{ 3.10 2.85 2.70 }"),
            Some([3.10, 2.85, 2.70])
        );
        assert_eq!(parse_load_average("1.0 x 2.0"), None);
        assert_eq!(parse_load_average("1.0"), None);
    }

    #[test]
    fn perf_csv_gives_the_instruction_count() {
        let text =
            "# started on Wed Nov 18 10:00:00 2026\n\n123456789,,instructions:u,1000,100.00,,\n";
        assert_eq!(parse_perf_csv(text), Some(123_456_789));
        assert_eq!(
            parse_perf_csv("<not counted>,,instructions:u,0,0.00,,\n"),
            None
        );
        assert_eq!(parse_perf_csv(""), None);
    }

    #[test]
    fn an_aarch64_cpu_is_named_by_its_codes() {
        let neoverse_n1 = "processor\t: 0\nBogoMIPS\t: 243.75\nFeatures\t: fp asimd evtstrm \
            aes pmull sha1 sha2 crc32 atomics fphp asimdhp cpuid asimdrdm lrcpc dcpop asimddp\n\
            CPU implementer\t: 0x41\nCPU architecture: 8\nCPU variant\t: 0x3\n\
            CPU part\t: 0xd0c\nCPU revision\t: 1\n\n";
        assert_eq!(field(neoverse_n1, "model name"), None);
        assert_eq!(arm_cpu_name(neoverse_n1).as_deref(), Some("Arm part 0xd0c"));
        let unknown_maker = "CPU implementer\t: 0x99\nCPU part\t: 0x001\n";
        assert_eq!(
            arm_cpu_name(unknown_maker).as_deref(),
            Some("implementer 0x99 part 0x001")
        );
        assert_eq!(arm_cpu_name("CPU implementer\t: 0x41\n"), None);
        assert_eq!(
            arm_cpu_name("CPU implementer\t: 41\nCPU part\t: 0xd0c\n"),
            None
        );
        assert_eq!(arm_cpu_name(""), None);
    }

    #[test]
    fn a_model_name_is_taken_from_cpuinfo_or_lscpu() {
        let x86 = "processor\t: 0\nvendor_id\t: AuthenticAMD\n\
            model name\t: AMD Ryzen AI Max+ 395\nflags\t\t: fpu vme\n";
        assert_eq!(field(x86, "model name"), Some("AMD Ryzen AI Max+ 395"));
        assert_eq!(field("model name\t:\n", "model name"), None);
        let lscpu = "Architecture:             aarch64\n  CPU op-mode(s):         64-bit\n\
            Vendor ID:                ARM\n  Model name:             Neoverse-N1\n";
        assert_eq!(lscpu_model_name(lscpu).as_deref(), Some("Neoverse-N1"));
        assert_eq!(lscpu_model_name("Model name: -\n"), None);
        assert_eq!(lscpu_model_name("Vendor ID: ARM\n"), None);
    }

    #[test]
    fn the_machine_describes_itself() {
        let machine = Machine::detect(Some("test".to_string()));
        assert_eq!(machine.label.as_deref(), Some("test"));
        assert_eq!(machine.os, std::env::consts::OS);
        assert!(machine.logical_cpus > 0);
        if cfg!(target_os = "linux") {
            assert!(machine.cpu.is_some());
            assert!(machine.memory_bytes.is_some());
            assert!(load_average().is_some());
            assert!(thread_cpu_ns().is_some());
            assert!(thread_id().is_some());
        }
    }

    #[test]
    fn the_commit_is_read_from_git_or_missing() {
        let here = Path::new(env!("CARGO_MANIFEST_DIR"));
        if let Some(commit) = source_commit(here) {
            assert!(commit.len() >= 40);
        }
        assert_eq!(source_commit(Path::new("/nonexistent/lignin")), None);
        assert_eq!(command_output("false", &[]), None);
    }

    /// Whether `dir` is on a file system that keeps its files in memory, a
    /// `tmpfs` or a `ramfs`, as `statfs` names the file system of the
    /// directory itself.
    fn in_memory(dir: &Path) -> bool {
        const TMPFS: i128 = 0x0102_1994;
        const RAMFS: i128 = 0x8584_58f6;
        let of_dir = rustix::fs::statfs(dir).expect("the file system of the directory");
        // The kind is a signed word on some machines.
        matches!(i128::from(of_dir.f_type) & 0xffff_ffff, TMPFS | RAMFS)
    }

    #[test]
    fn file_cache_drops_are_checked_before_they_are_used() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a"), b"data").unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        let method = ColdMethod::detect(dir.path());
        assert!(!dir.path().join("cold-probe").exists());
        if method == ColdMethod::ReopenAndDropFileCache {
            assert!(drop_dir_cache(dir.path()));
            assert!(drop_files_from_cache(&[dir.path().join("missing")]));
            assert!(process_io("read_bytes").is_some());
        }
        // Files that are kept in memory have no storage to be read from
        // again, so nothing is claimed of the method there.
        if cfg!(target_os = "linux")
            && Path::new("/usr/bin/python3").exists()
            && !in_memory(dir.path())
        {
            assert_eq!(method, ColdMethod::ReopenAndDropFileCache);
        }
        assert!(!drop_dir_cache(&dir.path().join("missing")));
        std::fs::write(dir.path().join("sub").join("b"), b"nested").unwrap();
        assert_eq!(
            files_under(dir.path()).unwrap(),
            vec![dir.path().join("a"), dir.path().join("sub").join("b")]
        );
        assert!(files_under(&dir.path().join("missing")).is_err());
        assert!(!cache_drop_works(&dir.path().join("missing").join("probe")));
        assert_eq!(process_io("no_such_field"), None);
    }

    #[test]
    fn a_tree_is_copied_with_its_directories() {
        let dir = tempfile::tempdir().unwrap();
        let from = dir.path().join("from");
        std::fs::create_dir_all(from.join("sub").join("empty")).unwrap();
        std::fs::write(from.join("a"), b"one").unwrap();
        std::fs::write(from.join("sub").join("b"), b"two").unwrap();
        let to = dir.path().join("to");
        copy_tree(&from, &to).unwrap();
        assert_eq!(std::fs::read(to.join("a")).unwrap(), b"one");
        assert_eq!(std::fs::read(to.join("sub").join("b")).unwrap(), b"two");
        assert!(to.join("sub").join("empty").is_dir());
        // A copy over what is there, and of what is not, is refused.
        assert!(copy_tree(&from, &to).is_err());
        assert!(copy_tree(&dir.path().join("missing"), &dir.path().join("other")).is_err());
    }

    #[test]
    fn the_reads_of_a_cgroup_are_summed_over_its_devices() {
        let text = "253:0 rbytes=4096 wbytes=8192 rios=1 wios=2 dbytes=0 dios=0\n\
                    8:16 rbytes=1000 wbytes=0 rios=3 wios=0 dbytes=0 dios=0\n";
        assert_eq!(device_reads(text), (4, 5_096));
        assert_eq!(device_reads(""), (0, 0));
        assert_eq!(device_reads("253:0 rios=many rbytes=7"), (0, 7));
    }

    #[test]
    fn storage_reads_are_differences_of_the_counts_both_readings_have() {
        let earlier = StorageReads {
            process_bytes: Some(10),
            process_calls: Some(4),
            device_reads: None,
            device_bytes: Some(100),
        };
        let later = StorageReads {
            process_bytes: Some(30),
            process_calls: Some(3),
            device_reads: Some(9),
            device_bytes: None,
        };
        assert_eq!(
            later.since(&earlier),
            StorageReads {
                process_bytes: Some(20),
                process_calls: Some(0),
                device_reads: None,
                device_bytes: None,
            }
        );
        // A reading takes the process's counts from the file `process_io`
        // reads, and both counts of the cgroup from one file.
        let now = StorageReads::now();
        assert_eq!(now.process_calls.is_some(), process_io("syscr").is_some());
        assert_eq!(now.device_reads.is_some(), now.device_bytes.is_some());
    }

    #[test]
    fn perf_counts_only_enabled_sections_where_it_works() {
        let (Some(tid), Ok(dir)) = (thread_id(), tempfile::tempdir()) else {
            return;
        };
        let Some(mut perf) = Perf::attach(tid, &dir.path().join("perf")) else {
            return;
        };
        assert!(perf.enable());
        let mut sum = 0u64;
        for n in 0..1_000_000u64 {
            sum = std::hint::black_box(sum.wrapping_add(n * n));
        }
        assert!(perf.disable());
        let count = perf.finish().expect("perf counted");
        assert!(
            count > 1_000_000,
            "{count} instructions for a million steps"
        );
    }
}
