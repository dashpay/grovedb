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

fn cpu_name() -> Option<String> {
    if let Ok(cpuinfo) = std::fs::read_to_string("/proc/cpuinfo") {
        return cpuinfo
            .lines()
            .find_map(|line| line.strip_prefix("model name"))
            .and_then(|rest| rest.split_once(':'))
            .map(|(_, name)| name.trim().to_string());
    }
    command_output("sysctl", &["-n", "machdep.cpu.brand_string"])
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

fn command_output(program: &str, args: &[&str]) -> Option<String> {
    let output = Command::new(program)
        .args(args)
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
