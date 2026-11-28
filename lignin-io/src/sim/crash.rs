//! What a crash leaves: the frozen file system, the plans that say which of
//! its unsynced changes survive, and the file system a plan gives.

use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    fmt,
    ops::Range,
    sync::Arc,
};

use super::{
    rng::{stream, Chance, SimRng},
    state::{
        check_one_more_fits, with_what_they_build_on, Dir, DirId, FileId, NameOp, Node, State, ROOT,
    },
    trace::{ChangeFate, CrashRecord, Event, Fate, NameFate},
    SimConfig, SimVfs,
};

/// How [`CrashImage::sample`] draws a crash state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CrashOdds {
    /// How often an unsynced change is lost.
    pub lose: Chance,
    /// How often a write that survives is torn.
    pub tear: Chance,
    /// How often a surviving change of a file is applied out of turn.
    pub reorder: Chance,
}

impl Default for CrashOdds {
    fn default() -> Self {
        Self {
            lose: Chance::new(1, 2),
            tear: Chance::new(1, 4),
            reorder: Chance::new(1, 8),
        }
    }
}

/// A change of a file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileChangeKind {
    /// `len` bytes written at `offset` by `write_at` or `append`.
    Write {
        /// Where the bytes start.
        offset: u64,
        /// How many there are, at least one.
        len: u64,
    },
    /// The length set to `len` by `set_len`, or by an append that failed and
    /// was taken back.
    SetLen {
        /// The length.
        len: u64,
    },
}

impl fmt::Display for FileChangeKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Write { offset, len } => write!(f, "write offset {offset} len {len}"),
            Self::SetLen { len } => write!(f, "set_len {len}"),
        }
    }
}

/// An unsynced change of a file, as a [`CrashImage`] lists it.
#[derive(Clone, PartialEq, Eq)]
pub struct FileChange {
    /// The number of the operation that made the change.
    pub op: u64,
    /// The path the file was opened by. The file may have other names by
    /// now, or none.
    pub path: String,
    /// The simulator's number for the file: changes with the same number are
    /// changes of one file.
    pub file: u64,
    /// The change.
    pub kind: FileChangeKind,
    /// How many sectors a write touches: the pieces it can tear into.
    pub sectors: u64,
    /// Whether a failed sync left this change unknown. A crash treats it like
    /// any other unsynced change.
    pub unknown: bool,
    pub(super) data: Arc<[u8]>,
}

impl FileChange {
    /// The bytes of a write. A change of length has none.
    pub fn data(&self) -> &[u8] {
        &self.data
    }
}

impl fmt::Debug for FileChange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{} {} {}", self.op, self.kind, self.path)
    }
}

/// What a change of names did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NameChangeKind {
    /// `open` or `lock` created a file.
    Create,
    /// `hard_link` added a name.
    Link,
    /// `remove_file` removed a name.
    Remove,
    /// `rename` moved a name: one change, which a crash keeps whole or not at
    /// all.
    Rename,
    /// `create_dir` created a directory.
    CreateDir,
    /// `remove_dir` removed a directory.
    RemoveDir,
}

/// An unsynced change of names, as a [`CrashImage`] lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NameChange {
    /// The number of the operation that made the change.
    pub op: u64,
    /// What it did.
    pub kind: NameChangeKind,
    /// The name it was made to. For a rename and a link, the existing name.
    pub path: String,
    /// The new name of a rename or a link.
    pub to: Option<String>,
    /// The simulator's number for the file whose name changed, as in
    /// [`FileChange::file`]. A change of a directory has none.
    pub file: Option<u64>,
}

impl fmt::Display for NameChange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let kind = match self.kind {
            NameChangeKind::Create => "create",
            NameChangeKind::Link => "hard_link",
            NameChangeKind::Remove => "remove_file",
            NameChangeKind::Rename => "rename",
            NameChangeKind::CreateDir => "create_dir",
            NameChangeKind::RemoveDir => "remove_dir",
        };
        write!(f, "#{} {kind} {}", self.op, self.path)?;
        match &self.to {
            Some(to) => write!(f, " -> {to}"),
            None => Ok(()),
        }
    }
}

/// The part of a write that survives a crash.
///
/// A write tears between sectors. Its sectors are counted from 0, the sector
/// its first byte is in, and each one survives whole or not at all. The
/// length the write gave the file is kept apart from the bytes, as a file
/// system keeps it: [`len`](Self::len) is the length the file is grown to
/// before the surviving sectors are put in, and a sector, or the part of one,
/// that is then still past the end of the file is dropped. Bytes that the
/// growth adds and no surviving sector fills read as zeros, and bytes the
/// file had that no surviving sector fills stay as they were.
///
/// For a write of sectors `0..4` that ends at byte `end`:
///
/// | Tear | What is in the file |
/// | --- | --- |
/// | `sectors: [0..2], len:` end of sector 1 | The first two sectors. |
/// | `sectors: [0..2], len: end` | The whole length. After the first two sectors it keeps what was there, and reads as zeros where the file had not reached. |
/// | `sectors: [], len: end` | The whole length, with nothing of the write: what was there, and zeros where the file had not reached. |
/// | `sectors: [0..4], len: 0` | The bytes that overwrote the file as it was, and no growth. |
/// | `sectors: [1..2, 3..4], len: end` | Two sectors, and gaps that keep what was there or read as zeros. |
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tear {
    /// The sectors of the write that survive, as rising ranges that do not
    /// overlap.
    pub sectors: Vec<Range<u64>>,
    /// The file is grown to this length if it is shorter. It is 0, for no
    /// growth, or lies inside the write at a sector boundary or at the
    /// write's end.
    pub len: u64,
}

impl Tear {
    /// The tear in which one run of sectors survives, or none if `sectors` is
    /// empty, with the file grown to `len`.
    pub fn run(sectors: Range<u64>, len: u64) -> Self {
        let sectors = if sectors.is_empty() {
            Vec::new()
        } else {
            Vec::from([sectors])
        };
        Self { sectors, len }
    }
}

impl fmt::Display for Tear {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("sectors")?;
        if self.sectors.is_empty() {
            f.write_str(" none")?;
        }
        for (index, sectors) in self.sectors.iter().enumerate() {
            let separator = if index == 0 { " " } else { ", " };
            write!(f, "{separator}{}..{}", sectors.start, sectors.end)?;
        }
        write!(f, ", length {}", self.len)
    }
}

/// A change of a file that survives a crash.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Survivor {
    /// The change, as its place in [`CrashImage::changes`].
    pub change: usize,
    /// The part of it that survives, for a write that does not survive
    /// whole.
    pub tear: Option<Tear>,
}

impl Survivor {
    /// The whole of change number `change`.
    pub fn whole(change: usize) -> Self {
        Self { change, tear: None }
    }
}

/// Which unsynced changes survive a crash.
///
/// Every change left out is lost. [`CrashImage`] makes the common plans, and
/// any other one can be written by hand:
///
/// ```
/// use lignin_io::{
///     sim::{CrashPlan, SimConfig, SimVfs, Survivor},
///     Create, OpenMode, Vfs,
/// };
/// use std::path::Path;
///
/// let fs = SimVfs::new(SimConfig::new(7));
/// let file = fs.open(Path::new("/file"), OpenMode::Write(Create::New))?;
/// fs.sync_dir(Path::new("/"))?;
/// file.write_at(b"old", 0)?;
/// file.write_at(b"new", 0)?;
///
/// // Both writes reach the disk, the older one last.
/// let image = fs.crash_image();
/// let plan = CrashPlan {
///     names: vec![],
///     changes: vec![Survivor::whole(1), Survivor::whole(0)],
/// };
/// let after = image.recover(&plan).unwrap();
/// let mut content = [0; 3];
/// after
///     .open(Path::new("/file"), OpenMode::Read)?
///     .read_exact_at(&mut content, 0)?;
/// assert_eq!(&content, b"old");
/// # Ok::<(), std::io::Error>(())
/// ```
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CrashPlan {
    /// The changes of names that survive, as their places in
    /// [`CrashImage::names`]. The changes they build on survive with them,
    /// listed or not: see [`CrashImage::names_kept`]. Their order here does
    /// not matter: names are always changed in the order the changes were
    /// made.
    pub names: Vec<usize>,
    /// The changes of files that survive, in the order they are applied.
    pub changes: Vec<Survivor>,
}

/// Why a [`CrashPlan`] does not fit its [`CrashImage`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InvalidPlan {
    reason: String,
}

impl fmt::Display for InvalidPlan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "the crash plan does not fit the image: {}", self.reason)
    }
}

impl Error for InvalidPlan {}

/// An unsynced change of a file. `handle` is the handle that made it.
#[derive(Clone, Debug)]
pub(super) struct Change {
    pub(super) op: u64,
    pub(super) path: Arc<str>,
    pub(super) handle: u64,
    pub(super) kind: FileChangeKind,
    pub(super) data: Arc<[u8]>,
}

fn sector_count(offset: u64, len: u64, sector: u64) -> u64 {
    (offset + len - 1) / sector - offset / sector + 1
}

fn sector_start(offset: u64, sector: u64, index: u64) -> u64 {
    if index == 0 {
        offset
    } else {
        (offset / sector + index).saturating_mul(sector)
    }
}

fn sector_end(offset: u64, len: u64, sector: u64, index: u64) -> u64 {
    (offset / sector + index + 1)
        .saturating_mul(sector)
        .min(offset + len)
}

pub(super) fn sectors_of(kind: FileChangeKind, sector: u64) -> u64 {
    match kind {
        FileChangeKind::Write { offset, len } => sector_count(offset, len, sector),
        FileChangeKind::SetLen { .. } => 0,
    }
}

/// Where an append that fails part of the way stops: after any whole number
/// of its sectors.
pub(super) fn cut(rng: &mut SimRng, offset: u64, len: u64, sector: u64) -> u64 {
    match rng.below(sector_count(offset, len, sector) + 1) {
        0 => 0,
        kept => sector_end(offset, len, sector, kept - 1) - offset,
    }
}

/// The parts of a write that fails part of the way which were written, as
/// ranges of bytes of the file: its first sectors, or any of its sectors.
pub(super) fn failed_write(
    rng: &mut SimRng,
    offset: u64,
    len: u64,
    sector: u64,
) -> Vec<Range<u64>> {
    let count = sector_count(offset, len, sector);
    let sectors = match rng.below(2) {
        0 => Tear::run(0..rng.below(count + 1), 0).sectors,
        _ => some_sectors(rng, count),
    };
    let bytes = |sectors: Range<u64>| {
        sector_start(offset, sector, sectors.start)
            ..sector_end(offset, len, sector, sectors.end - 1)
    };
    sectors.into_iter().map(bytes).collect()
}

/// Each of `count` sectors or not, as it falls.
fn some_sectors(rng: &mut SimRng, count: u64) -> Vec<Range<u64>> {
    let mut sectors: Vec<Range<u64>> = Vec::new();
    for index in 0..count {
        if rng.below(2) == 0 {
            continue;
        }
        match sectors.last_mut() {
            Some(last) if last.end == index => last.end = index + 1,
            _ => sectors.push(index..index + 1),
        }
    }
    sectors
}

pub(super) fn apply(
    image: &mut Vec<u8>,
    kind: FileChangeKind,
    data: &[u8],
    tear: Option<&Tear>,
    sector: u64,
) {
    match (kind, tear) {
        (FileChangeKind::SetLen { len }, _) => image.resize(len as usize, 0),
        (FileChangeKind::Write { offset, len }, None) => {
            let (start, end) = (offset as usize, (offset + len) as usize);
            if image.len() < end {
                image.resize(end, 0);
            }
            image[start..end].copy_from_slice(data);
        }
        (FileChangeKind::Write { offset, len }, Some(tear)) => {
            if (image.len() as u64) < tear.len {
                image.resize(tear.len as usize, 0);
            }
            for sectors in &tear.sectors {
                let from = sector_start(offset, sector, sectors.start);
                let to = sector_end(offset, len, sector, sectors.end - 1).min(image.len() as u64);
                if from < to {
                    image[from as usize..to as usize]
                        .copy_from_slice(&data[(from - offset) as usize..(to - offset) as usize]);
                }
            }
        }
    }
}

fn random_tear(rng: &mut SimRng, offset: u64, len: u64, sector: u64) -> Tear {
    let count = sector_count(offset, len, sector);
    let end = offset + len;
    let reach = |kept: u64| match kept {
        0 => 0,
        kept => sector_end(offset, len, sector, kept - 1),
    };
    match rng.below(4) {
        0 => {
            let kept = rng.below(count);
            Tear::run(0..kept, reach(kept))
        }
        1 => Tear::run(0..rng.below(count), end),
        2 => {
            let sectors = some_sectors(rng, count);
            let len = match rng.below(2) {
                0 => reach(sectors.last().map_or(0, |last| last.end)),
                _ => end,
            };
            Tear { sectors, len }
        }
        _ => Tear::run(0..count, 0),
    }
}

pub(super) fn sample_changes(
    rng: &mut SimRng,
    kinds: impl Iterator<Item = FileChangeKind>,
    odds: &CrashOdds,
    sector: u64,
) -> Vec<Survivor> {
    let mut survivors = Vec::new();
    for (change, kind) in kinds.enumerate() {
        if rng.chance(odds.lose) {
            continue;
        }
        let tear = match kind {
            FileChangeKind::Write { offset, len } if rng.chance(odds.tear) => {
                Some(random_tear(rng, offset, len, sector))
            }
            _ => None,
        };
        survivors.push(Survivor { change, tear });
    }
    for index in 0..survivors.len() {
        if rng.chance(odds.reorder) {
            let other = rng.below(survivors.len() as u64) as usize;
            survivors.swap(index, other);
        }
    }
    survivors
}

pub(super) fn fates<'a>(
    changes: impl Iterator<Item = (u64, &'a str, FileChangeKind)>,
    survivors: &[Survivor],
) -> Vec<ChangeFate> {
    let mut fates: Vec<ChangeFate> = changes
        .map(|(op, path, kind)| ChangeFate {
            op,
            path: path.to_owned(),
            kind,
            fate: Fate::Lost,
        })
        .collect();
    for (position, survivor) in survivors.iter().enumerate() {
        fates[survivor.change].fate = Fate::Survives {
            position: position as u64,
            tear: survivor.tear.clone(),
        };
    }
    fates
}

#[derive(Clone, Copy)]
enum Pending {
    Name(usize),
    Change(usize),
}

fn plan_of(pending: impl Iterator<Item = Pending>) -> CrashPlan {
    let mut plan = CrashPlan::default();
    for item in pending {
        match item {
            Pending::Name(name) => plan.names.push(name),
            Pending::Change(change) => plan.changes.push(Survivor::whole(change)),
        }
    }
    plan
}

/// A file system frozen at the moment of a crash: what is durable, and the
/// unsynced changes a crash may keep or lose.
///
/// [`SimVfs::crash_image`] takes one. A [`CrashPlan`] picks one of the
/// states the crash can leave, and [`recover`](Self::recover) gives that
/// state as a new [`SimVfs`]. Taking an image and recovering from it leave
/// the file system they came from as it was, so one image can be recovered
/// from under any number of plans.
#[derive(Clone)]
pub struct CrashImage {
    pub(super) config: SimConfig,
    pub(super) ops: u64,
    pub(super) dirs: BTreeMap<DirId, Dir>,
    pub(super) name_ops: BTreeMap<u64, NameOp>,
    pub(super) files: BTreeMap<FileId, Arc<Vec<u8>>>,
    pub(super) changes: Vec<FileChange>,
    pub(super) names: Vec<NameChange>,
}

impl fmt::Debug for CrashImage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CrashImage")
            .field("seed", &self.config.seed)
            .field("ops", &self.ops)
            .field("names", &self.names)
            .field("changes", &self.changes)
            .finish_non_exhaustive()
    }
}

impl CrashImage {
    /// The seed of the file system the image was taken from.
    pub fn seed(&self) -> u64 {
        self.config.seed
    }

    /// How many operations that file system had made.
    pub fn ops(&self) -> u64 {
        self.ops
    }

    /// The sector size writes tear at.
    pub fn sector_size(&self) -> u64 {
        self.config.sector_size
    }

    /// The unsynced changes of files, in the order they were made.
    pub fn changes(&self) -> &[FileChange] {
        &self.changes
    }

    /// The unsynced changes of names, in the order they were made.
    pub fn names(&self) -> &[NameChange] {
        &self.names
    }

    /// The changes of names that survive under `plan`: the ones it lists and
    /// the ones those build on, as rising places in [`names`](Self::names).
    ///
    /// # Panics
    ///
    /// If `plan` lists a place that is not in [`names`](Self::names).
    pub fn names_kept(&self, plan: &CrashPlan) -> Vec<usize> {
        let listed = plan.names.iter().map(|name| self.names[*name].op);
        let kept = with_what_they_build_on(&self.dirs, &self.name_ops, listed);
        (0..self.names.len())
            .filter(|name| kept.contains(&self.names[*name].op))
            .collect()
    }

    /// The plan in which no unsynced change survives: the file system as it
    /// was last synced.
    pub fn lose_all(&self) -> CrashPlan {
        CrashPlan::default()
    }

    /// The plan in which every unsynced change survives whole and in order.
    pub fn keep_all(&self) -> CrashPlan {
        plan_of(self.order().into_iter())
    }

    /// The plan in which the changes made by operations up to number `op`
    /// survive whole and in order, and the later ones are lost: what a file
    /// system that writes everything in order leaves.
    pub fn keep_through(&self, op: u64) -> CrashPlan {
        plan_of(
            self.order()
                .into_iter()
                .filter(|item| self.op_of(*item) <= op),
        )
    }

    /// A plan drawn from the seed with the file system's own
    /// [`CrashOdds`]. `draw` picks one of the many plans the seed gives for
    /// this image: the same `draw` gives the same plan.
    pub fn sample(&self, draw: u64) -> CrashPlan {
        self.sample_with(draw, &self.config.crash)
    }

    /// A plan drawn from the seed with `odds`.
    pub fn sample_with(&self, draw: u64, odds: &CrashOdds) -> CrashPlan {
        let at_crash = SimRng::derive(self.config.seed, stream::CRASH, self.ops).next_u64();
        let mut rng = SimRng::derive(at_crash, stream::DRAW, draw);
        let names = (0..self.names.len())
            .filter(|_| !rng.chance(odds.lose))
            .collect();
        let changes = sample_changes(
            &mut rng,
            self.changes.iter().map(|change| change.kind),
            odds,
            self.config.sector_size,
        );
        let mut plan = CrashPlan { names, changes };
        plan.names = self.names_kept(&plan);
        plan
    }

    /// The ways the write at place `change` of [`changes`](Self::changes)
    /// can be cut short, for writing plans by hand. For each number of
    /// leading sectors short of all of them: those sectors with the length
    /// they need, and those sectors with the whole length of the write. Last
    /// comes every sector with no growth of the file. A change that is not a
    /// write has none.
    ///
    /// # Panics
    ///
    /// If `change` is not a place in [`changes`](Self::changes).
    pub fn tears(&self, change: usize) -> Vec<Tear> {
        let FileChangeKind::Write { offset, len } = self.changes[change].kind else {
            return Vec::new();
        };
        let sector = self.config.sector_size;
        let count = sector_count(offset, len, sector);
        let mut tears = Vec::new();
        for kept in 0..count {
            if kept > 0 {
                let reach = sector_end(offset, len, sector, kept - 1);
                tears.push(Tear::run(0..kept, reach));
            }
            tears.push(Tear::run(0..kept, offset + len));
        }
        tears.push(Tear::run(0..count, 0));
        tears
    }

    /// The plans of a file system that writes in order: for every point in
    /// the sequence of unsynced changes, the plan in which everything before
    /// it survives and everything after is lost, followed, if the first lost
    /// change is a write, by that plan with each of the write's
    /// [`tears`](Self::tears).
    pub fn prefixes(&self) -> impl Iterator<Item = CrashPlan> + '_ {
        let order = self.order();
        (0..=order.len()).flat_map(move |kept| {
            let whole = plan_of(order[..kept].iter().copied());
            let mut torn = Vec::new();
            if let Some(Pending::Change(change)) = order.get(kept).copied() {
                for tear in self.tears(change) {
                    let mut plan = whole.clone();
                    plan.changes.push(Survivor {
                        change,
                        tear: Some(tear),
                    });
                    torn.push(plan);
                }
            }
            std::iter::once(whole).chain(torn)
        })
    }

    /// The plans in which one unsynced change is lost, with the changes of
    /// names that build on it, and every other one survives.
    pub fn holes(&self) -> impl Iterator<Item = CrashPlan> + '_ {
        let order = self.order();
        (0..order.len()).map(move |lost| {
            let builds_on_lost = |item: &Pending| match (order[lost], item) {
                (Pending::Name(lost), Pending::Name(name)) => {
                    let alone = CrashPlan {
                        names: vec![*name],
                        changes: Vec::new(),
                    };
                    self.names_kept(&alone).contains(&lost)
                }
                (Pending::Change(lost), Pending::Change(change)) => lost == *change,
                _ => false,
            };
            plan_of(order.iter().filter(|item| !builds_on_lost(item)).copied())
        })
    }

    /// The plans in which everything survives but one change of a file is
    /// applied late, after a later change of the same file that it overlaps:
    /// what a device that reorders writes leaves.
    pub fn reorders(&self) -> impl Iterator<Item = CrashPlan> + '_ {
        let mut pairs = Vec::new();
        for (late, later_change) in self.changes.iter().enumerate() {
            for (early, change) in self.changes[..late].iter().enumerate() {
                if change.file == later_change.file && overlap(change.kind, later_change.kind) {
                    pairs.push((early, late));
                }
            }
        }
        pairs.into_iter().map(move |(early, late)| {
            let mut plan = self.keep_all();
            let moved = plan.changes.remove(early);
            plan.changes.insert(late, moved);
            plan
        })
    }

    /// Every plan there is, if there are at most `limit` of them: every set
    /// of changes of names that can survive, with every choice of surviving
    /// changes of files, in every order, each write whole or with every tear
    /// it can have. Between them they give every state this crash can leave.
    ///
    /// The count grows fast: a handful of small changes already has
    /// thousands of plans. `None` means there are more than `limit`.
    pub fn every_plan(&self, limit: usize) -> Option<Vec<CrashPlan>> {
        let limit = limit as u64;
        if self.names.len() >= 32 || 1 << self.names.len() > limit {
            return None;
        }
        let mut names = BTreeSet::new();
        for mask in 0..1usize << self.names.len() {
            let listed = (0..self.names.len()).filter(|name| mask >> name & 1 == 1);
            names.insert(self.names_kept(&CrashPlan {
                names: listed.collect(),
                changes: Vec::new(),
            }));
        }

        let mut ways = Vec::new();
        for change in &self.changes {
            let mut tears = vec![None];
            if let FileChangeKind::Write { offset, len } = change.kind {
                let end = offset + len;
                let lens: Vec<u64> = (1..change.sectors)
                    .map(|sectors| sector_end(offset, len, self.config.sector_size, sectors - 1))
                    .chain([0, end])
                    .collect();
                if change.sectors >= 32 || (lens.len() as u64) << change.sectors > limit {
                    return None;
                }
                for mask in 0..1u64 << change.sectors {
                    let mut sectors: Vec<Range<u64>> = Vec::new();
                    for index in (0..change.sectors).filter(|index| mask >> index & 1 == 1) {
                        match sectors.last_mut() {
                            Some(last) if last.end == index => last.end = index + 1,
                            _ => sectors.push(index..index + 1),
                        }
                    }
                    let torn = |len: &u64| Tear {
                        sectors: sectors.clone(),
                        len: *len,
                    };
                    tears.extend(lens.iter().map(torn).map(Some));
                }
            }
            ways.push(tears);
        }

        let mut orders = Vec::new();
        let limit = limit / names.len() as u64;
        if ways.len() >= 64 || 1 << ways.len() > limit {
            return None;
        }
        every_order(&ways, &mut Vec::new(), &mut orders, limit).then_some(())?;
        let mut plans = Vec::with_capacity(names.len() * orders.len());
        for names in names {
            plans.extend(orders.iter().map(|changes| CrashPlan {
                names: names.clone(),
                changes: changes.clone(),
            }));
        }
        Some(plans)
    }

    /// The file system this crash leaves under `plan`, as a new [`SimVfs`]
    /// with nothing open and nothing locked.
    ///
    /// A change of names survives together with the changes it builds on,
    /// whether `plan` lists them or not: see
    /// [`names_kept`](Self::names_kept).
    ///
    /// The new file system has the configuration of the one that crashed,
    /// the odds of faults and of write-backs included, and a seed derived
    /// from that one's seed and its number of operations. It counts its
    /// operations from 1 again. Its trace starts with the [`CrashRecord`] of
    /// `plan`. Rules and a crash point are not carried over, and no handle
    /// of the new file system is told of a write-back that failed in the
    /// old one.
    pub fn recover(&self, plan: &CrashPlan) -> Result<SimVfs, InvalidPlan> {
        self.check(plan)?;
        let kept: BTreeSet<u64> = self
            .names_kept(plan)
            .into_iter()
            .map(|name| self.names[name].op)
            .collect();
        let mut survivors: BTreeMap<FileId, Vec<&Survivor>> = BTreeMap::new();
        for survivor in &plan.changes {
            let file = FileId(self.changes[survivor.change].file);
            survivors.entry(file).or_default().push(survivor);
        }

        let seed = SimRng::derive(self.config.seed, stream::CHILD, self.ops).next_u64();
        let mut state = State::new(SimConfig {
            seed,
            ..self.config.clone()
        });
        state.trace.push(Event::Crash(self.record(plan, &kept)));
        self.rebuild(&kept, &survivors, &mut state);
        Ok(SimVfs::from_state(state))
    }

    /// Gives `state` the names the crash leaves, from the root down: each
    /// directory's before the next name of the directory above it.
    fn rebuild(
        &self,
        kept: &BTreeSet<u64>,
        survivors: &BTreeMap<FileId, Vec<&Survivor>>,
        state: &mut State,
    ) {
        let mut files = BTreeMap::new();
        let mut open = vec![(ROOT, self.dirs[&ROOT].cells.iter())];
        while let Some((into, cells)) = open.last_mut() {
            let into = *into;
            let Some((name, cell)) = cells.next() else {
                open.pop();
                continue;
            };
            let surviving = cell
                .pending
                .iter()
                .take_while(|(seq, _)| kept.contains(seq));
            match surviving.last().map_or(cell.durable, |(_, node)| *node) {
                None => {}
                Some(Node::File(file)) => {
                    let file = *files.entry(file).or_insert_with(|| {
                        state.add_file(self.content(file, survivors.get(&file)))
                    });
                    state.place(into, name, Node::File(file));
                }
                Some(Node::Dir(dir)) => {
                    let new = state.add_dir(into, name);
                    state.place(into, name, Node::Dir(new));
                    check_one_more_fits(open.len(), &self.dirs);
                    open.push((new, self.dirs[&dir].cells.iter()));
                }
            }
        }
    }

    fn content(&self, file: FileId, survivors: Option<&Vec<&Survivor>>) -> Arc<Vec<u8>> {
        let durable = &self.files[&file];
        let Some(survivors) = survivors else {
            return durable.clone();
        };
        let mut content = Vec::clone(durable);
        for survivor in survivors {
            let change = &self.changes[survivor.change];
            apply(
                &mut content,
                change.kind,
                &change.data,
                survivor.tear.as_ref(),
                self.config.sector_size,
            );
        }
        Arc::new(content)
    }

    fn record(&self, plan: &CrashPlan, kept: &BTreeSet<u64>) -> CrashRecord {
        let names = self
            .names
            .iter()
            .map(|change| NameFate {
                change: change.clone(),
                survives: kept.contains(&change.op),
            })
            .collect();
        let changes = fates(
            self.changes
                .iter()
                .map(|change| (change.op, change.path.as_str(), change.kind)),
            &plan.changes,
        );
        CrashRecord {
            seed: self.config.seed,
            ops: self.ops,
            names,
            changes,
        }
    }

    fn check(&self, plan: &CrashPlan) -> Result<(), InvalidPlan> {
        let invalid = |reason: String| Err(InvalidPlan { reason });
        let mut names = BTreeSet::new();
        for name in &plan.names {
            if *name >= self.names.len() {
                return invalid(format!("there is no change of names at place {name}"));
            }
            if !names.insert(*name) {
                return invalid(format!("the change of names at place {name} is kept twice"));
            }
        }
        let mut changes = BTreeSet::new();
        for survivor in &plan.changes {
            let place = survivor.change;
            let Some(change) = self.changes.get(place) else {
                return invalid(format!("there is no change of a file at place {place}"));
            };
            if !changes.insert(place) {
                return invalid(format!(
                    "the change of a file at place {place} is kept twice"
                ));
            }
            let Some(tear) = &survivor.tear else {
                continue;
            };
            let FileChangeKind::Write { offset, len } = change.kind else {
                return invalid(format!("the change at place {place} is not a write"));
            };
            let mut next = 0;
            for sectors in &tear.sectors {
                if sectors.start < next
                    || sectors.start >= sectors.end
                    || sectors.end > change.sectors
                {
                    return invalid(format!(
                        "the tear of the write at place {place} names sectors {}..{}, which are \
                         not rising ranges within its {} sectors",
                        sectors.start, sectors.end, change.sectors
                    ));
                }
                next = sectors.end;
            }
            let end = offset + len;
            let inside = tear.len > offset && tear.len <= end;
            let on_boundary = tear.len.is_multiple_of(self.config.sector_size) || tear.len == end;
            if tear.len != 0 && !(inside && on_boundary) {
                return invalid(format!(
                    "the tear of the write at place {place} has length {}, which is neither 0 nor \
                     a sector boundary or the end of the write within {offset}..={end}",
                    tear.len
                ));
            }
        }
        Ok(())
    }

    fn order(&self) -> Vec<Pending> {
        let mut order: Vec<Pending> = (0..self.names.len())
            .map(Pending::Name)
            .chain((0..self.changes.len()).map(Pending::Change))
            .collect();
        order.sort_by_key(|item| self.op_of(*item));
        order
    }

    fn op_of(&self, item: Pending) -> u64 {
        match item {
            Pending::Name(name) => self.names[name].op,
            Pending::Change(change) => self.changes[change].op,
        }
    }
}

/// Adds to `orders` every choice of changes, each in one of its `ways` to
/// survive, in every order, on top of `current`. Says whether they fit in
/// `limit`.
///
/// It calls itself once for each change of an order. The caller gives it
/// `ways` only if the choices of changes alone fit in `limit`, each kept or
/// not: two to the power of their number. So an order has fewer than 64
/// changes.
fn every_order(
    ways: &[Vec<Option<Tear>>],
    current: &mut Vec<Survivor>,
    orders: &mut Vec<Vec<Survivor>>,
    limit: u64,
) -> bool {
    if orders.len() as u64 >= limit {
        return false;
    }
    orders.push(current.clone());
    for (change, tears) in ways.iter().enumerate() {
        if current.iter().any(|survivor| survivor.change == change) {
            continue;
        }
        for tear in tears {
            current.push(Survivor {
                change,
                tear: tear.clone(),
            });
            let fits = every_order(ways, current, orders, limit);
            current.pop();
            if !fits {
                return false;
            }
        }
    }
    true
}

fn overlap(early: FileChangeKind, late: FileChangeKind) -> bool {
    match (early, late) {
        (
            FileChangeKind::Write { offset, len },
            FileChangeKind::Write {
                offset: late_offset,
                len: late_len,
            },
        ) => offset < late_offset + late_len && late_offset < offset + len,
        _ => true,
    }
}
