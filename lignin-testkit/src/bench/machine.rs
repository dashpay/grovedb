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

/// How a cold read is made cold.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ColdMethod {
    /// The database is closed and opened again, which empties the engine's
    /// own caches. The operating system's page cache keeps the files.
    Reopen,
    /// As [`ColdMethod::Reopen`], and while it is closed every file of the
    /// database is dropped from the page cache with
    /// `dd if=<file> iflag=nocache count=0`, which asks the kernel
    /// (`posix_fadvise(POSIX_FADV_DONTNEED)`) to drop the file's clean
    /// pages. Needs no privilege, unlike `/proc/sys/vm/drop_caches`.
    ReopenAndDropFileCache,
}

impl ColdMethod {
    /// The strongest method this machine allows: dropping files from the
    /// page cache where `dd` can, reopening only elsewhere.
    pub fn detect(scratch: &Path) -> ColdMethod {
        let probe = scratch.join("cold-probe");
        let works = std::fs::write(&probe, [0u8; 4096]).is_ok() && drop_file_cache(&probe);
        let _ = std::fs::remove_file(&probe);
        if works {
            ColdMethod::ReopenAndDropFileCache
        } else {
            ColdMethod::Reopen
        }
    }
}

/// Drops `file` from the page cache. Returns false if `dd` refused.
pub fn drop_file_cache(file: &Path) -> bool {
    let Some(file) = file.to_str() else {
        return false;
    };
    Command::new("dd")
        .args([&format!("if={file}"), "iflag=nocache", "count=0", "status=none"])
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// Drops every file in `dir` from the page cache. Returns false if any
/// could not be dropped.
pub fn drop_dir_cache(dir: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    let mut all = true;
    for entry in entries.flatten() {
        if entry.file_type().is_ok_and(|kind| kind.is_file()) {
            all &= drop_file_cache(&entry.path());
        }
    }
    all
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
        let text = "# started on Wed Nov 18 10:00:00 2026\n\n123456789,,instructions:u,1000,100.00,,\n";
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

    #[test]
    fn file_cache_drops_report_what_they_did() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!drop_file_cache(&dir.path().join("missing")));
        std::fs::write(dir.path().join("a"), b"data").unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        let method = ColdMethod::detect(dir.path());
        assert_eq!(
            drop_dir_cache(dir.path()),
            method == ColdMethod::ReopenAndDropFileCache
        );
        assert!(!drop_dir_cache(&dir.path().join("missing")));
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
        assert!(count > 1_000_000, "{count} instructions for a million steps");
    }
}
