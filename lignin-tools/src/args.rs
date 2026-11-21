//! The arguments of the command line, parsed by hand.

use std::{ffi::OsString, path::PathBuf, time::Duration};

use lignin_format::PageId;
use lignindb::txn::Subtree;

use crate::ToolError;

/// What `lignin help`, and an argument that is no subcommand, print.
pub const USAGE: &str = "\
usage: lignin SUBCOMMAND DIR [OPTIONS]

  manifest  DIR                      the manifest: format, page size, live files, table checkpoint
  segments  DIR [--blocks]           each segment the manifest names; --blocks lists every block
  pages     DIR [PAGE...] [--cells]  pages of the latest version, every page with none named;
                                     --cells lists the cells and entries of each
  directory DIR                      every entry of the subtree directory
  verify    DIR [--no-hashes] [--beside-background]
                                     the physical invariant checker; every stored hash is made
                                     again by Merk's rules unless --no-hashes. The check runs on
                                     the still store at the close, or with --beside-background
                                     beside the background work of the open, as a node runs it;
                                     reclaiming waits while a check runs
  stats     DIR                      every count of the engine after an open
  compact   DIR [--timeout SECS]     reclaim and clean until a whole pass of the cleaner moves
                                     nothing (at most 600 s unless given)
  dump      DIR (SUBTREE | --all) FILE
                                     a subtree with every subtree below it, or every subtree, to FILE
  load      DIR FILE                 a dump, into a database that has none of its subtrees
  help                               this text

Every subcommand that opens DIR takes --cache MIB, the buffer pool (64 MiB unless given).
A SUBTREE is its 32 bytes in hex, as `lignin directory` prints them.
Nothing opens a directory that a process has open: run on a stopped node or on a checkpoint.
";

/// The buffer pool of an open unless `--cache` says another size.
pub const DEFAULT_CACHE: usize = 64 << 20;

/// How long `compact` works at most unless `--timeout` says.
pub const DEFAULT_COMPACT_TIMEOUT: Duration = Duration::from_secs(600);

/// What every subcommand that opens the database is given.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Common {
    /// The database directory.
    pub dir: PathBuf,
    /// The bytes of the buffer pool.
    pub cache_size: usize,
}

/// What `dump` writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DumpWhat {
    /// One subtree, and every subtree below it.
    Subtree(Subtree),
    /// Every subtree that no element hosts, each with every subtree below
    /// it: the whole database.
    All,
}

/// A subcommand with its arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// `help`.
    Help,
    /// `manifest`.
    Manifest {
        /// The database directory.
        dir: PathBuf,
    },
    /// `segments`.
    Segments {
        /// The database directory.
        dir: PathBuf,
        /// Whether every block is listed.
        blocks: bool,
    },
    /// `pages`.
    Pages {
        /// The directory and the pool.
        common: Common,
        /// The pages to show; every page with none.
        pages: Vec<PageId>,
        /// Whether the cells and entries of each page are listed.
        cells: bool,
    },
    /// `directory`.
    Directory {
        /// The directory and the pool.
        common: Common,
    },
    /// `verify`.
    Verify {
        /// The directory and the pool.
        common: Common,
        /// Whether every stored hash is made again by Merk's rules.
        hashes: bool,
        /// Whether the check runs beside the background work of the open
        /// ([`lignindb::Db::verify`]) and not on the still store at the
        /// close ([`lignindb::Db::close_and_verify`]).
        beside_background: bool,
    },
    /// `stats`.
    Stats {
        /// The directory and the pool.
        common: Common,
    },
    /// `compact`.
    Compact {
        /// The directory and the pool.
        common: Common,
        /// How long it works at most.
        timeout: Duration,
    },
    /// `dump`.
    Dump {
        /// The directory and the pool.
        common: Common,
        /// What is dumped.
        what: DumpWhat,
        /// The file the dump is written to. It must not be there.
        file: PathBuf,
    },
    /// `load`.
    Load {
        /// The directory and the pool.
        common: Common,
        /// The dump.
        file: PathBuf,
    },
}

fn usage(what: impl Into<String>) -> ToolError {
    ToolError::Usage(what.into())
}

/// Reads `text` as a subtree: 64 hex digits.
pub fn subtree(text: &str) -> Result<Subtree, ToolError> {
    let bad = || usage(format!("{text:?} is not a subtree: 64 hex digits"));
    if text.len() != 2 * Subtree::LEN || !text.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(bad());
    }
    let mut prefix = [0; Subtree::LEN];
    for (at, byte) in prefix.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&text[2 * at..2 * at + 2], 16).map_err(|_| bad())?;
    }
    Ok(Subtree::new(prefix))
}

fn number<T: std::str::FromStr>(text: &str, what: &str) -> Result<T, ToolError> {
    text.parse()
        .map_err(|_| usage(format!("{text:?} is not {what}")))
}

/// Parses the arguments after the program's name.
pub fn parse(args: &[OsString]) -> Result<Command, ToolError> {
    let mut words = Vec::with_capacity(args.len());
    for arg in args {
        let word = arg
            .to_str()
            .ok_or_else(|| usage(format!("{arg:?} is not UTF-8")))?;
        words.push(word);
    }
    let Some((&name, rest)) = words.split_first() else {
        return Err(usage("no subcommand"));
    };
    let mut flags = Flags::default();
    let mut operands = Vec::new();
    let mut rest = rest.iter();
    while let Some(&word) = rest.next() {
        let mut value = |flag: &str| {
            rest.next()
                .copied()
                .ok_or_else(|| usage(format!("{flag} takes a value")))
        };
        match word {
            "--blocks" => flags.blocks = true,
            "--cells" => flags.cells = true,
            "--no-hashes" => flags.no_hashes = true,
            "--beside-background" => flags.beside_background = true,
            "--all" => flags.all = true,
            "--cache" => {
                let mib: usize = number(value(word)?, "a size in MiB")?;
                let bytes = mib.checked_mul(1 << 20).filter(|&bytes| bytes > 0);
                flags.cache = Some(bytes.ok_or_else(|| usage("--cache takes 1 MiB or more"))?);
            }
            "--timeout" => {
                let seconds: u64 = number(value(word)?, "a number of seconds")?;
                flags.timeout = Some(Duration::from_secs(seconds));
            }
            flag if flag.starts_with("--") => return Err(usage(format!("no option {flag}"))),
            operand => operands.push(operand),
        }
    }
    let command = flags.command(name, &operands)?;
    Ok(command)
}

#[derive(Debug, Default)]
struct Flags {
    blocks: bool,
    cells: bool,
    no_hashes: bool,
    beside_background: bool,
    all: bool,
    cache: Option<usize>,
    timeout: Option<Duration>,
}

impl Flags {
    fn command(self, name: &str, operands: &[&str]) -> Result<Command, ToolError> {
        let common = |dir: &str| Common {
            dir: PathBuf::from(dir),
            cache_size: self.cache.unwrap_or(DEFAULT_CACHE),
        };
        let opens = !matches!(name, "help" | "manifest" | "segments");
        let allowed: &[(&str, bool)] = &[
            ("--blocks", self.blocks && name != "segments"),
            ("--cells", self.cells && name != "pages"),
            ("--no-hashes", self.no_hashes && name != "verify"),
            (
                "--beside-background",
                self.beside_background && name != "verify",
            ),
            ("--all", self.all && name != "dump"),
            ("--cache", self.cache.is_some() && !opens),
            ("--timeout", self.timeout.is_some() && name != "compact"),
        ];
        if let Some((flag, _)) = allowed.iter().find(|(_, misplaced)| *misplaced) {
            return Err(usage(format!("{name} takes no {flag}")));
        }
        fn one<'a>(name: &str, operands: &[&'a str]) -> Result<&'a str, ToolError> {
            match operands {
                [dir] => Ok(dir),
                _ => Err(usage(format!("{name} takes one directory"))),
            }
        }
        Ok(match name {
            "help" | "--help" | "-h" if operands.is_empty() => Command::Help,
            "manifest" => Command::Manifest {
                dir: PathBuf::from(one(name, operands)?),
            },
            "segments" => Command::Segments {
                dir: PathBuf::from(one(name, operands)?),
                blocks: self.blocks,
            },
            "pages" => {
                let Some((dir, pages)) = operands.split_first() else {
                    return Err(usage("pages takes a directory"));
                };
                let pages = pages.iter().map(|page| {
                    let raw: u32 = number(page, "a page id")?;
                    PageId::new(raw).ok_or_else(|| usage("page ids begin at 1"))
                });
                Command::Pages {
                    common: common(dir),
                    pages: pages.collect::<Result<_, _>>()?,
                    cells: self.cells,
                }
            }
            "directory" => Command::Directory {
                common: common(one(name, operands)?),
            },
            "verify" => Command::Verify {
                common: common(one(name, operands)?),
                hashes: !self.no_hashes,
                beside_background: self.beside_background,
            },
            "stats" => Command::Stats {
                common: common(one(name, operands)?),
            },
            "compact" => Command::Compact {
                common: common(one(name, operands)?),
                timeout: self.timeout.unwrap_or(DEFAULT_COMPACT_TIMEOUT),
            },
            "dump" => {
                let (dir, what, file) = match (self.all, operands) {
                    (true, [dir, file]) => (*dir, DumpWhat::All, *file),
                    (false, [dir, prefix, file]) => {
                        (*dir, DumpWhat::Subtree(subtree(prefix)?), *file)
                    }
                    _ => return Err(usage("dump takes DIR, a SUBTREE or --all, and FILE")),
                };
                Command::Dump {
                    common: common(dir),
                    what,
                    file: PathBuf::from(file),
                }
            }
            "load" => match operands {
                [dir, file] => Command::Load {
                    common: common(dir),
                    file: PathBuf::from(file),
                },
                _ => return Err(usage("load takes DIR and FILE")),
            },
            other => return Err(usage(format!("no subcommand {other:?}"))),
        })
    }
}
