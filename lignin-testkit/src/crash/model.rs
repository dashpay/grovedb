//! The model: what the pages of a store say after each item of its log that
//! changes them, kept apart from the store.
//!
//! The model knows no frame, block, chain or file. A page is its cells, a
//! cell is its key, its hashes and its value, and a value is its bytes
//! wherever the store keeps them. [`apply`] makes the changes of a commit
//! to that, with maps and nothing of `lignin-format`, so a mistake in how
//! the store writes, replays or loads a page shows as a difference.

use std::{collections::BTreeMap, fmt, sync::Arc};

use lignin_format::{wal::LogPlace, PageId};

/// A hash of a cell.
pub type Hash = [u8; 32];

/// The value of a cell of a tree page or of an entry of a flat leaf.
///
/// Whether a value is stored in its cell or out of line is part of what a
/// page says: a commit decides it, and nothing changes it afterwards. Where
/// a value out of line lies (in the log group of its commit, or in the
/// value log once write-back has placed it, or after a cleaner moved it)
/// is not.
#[derive(Clone, PartialEq, Eq)]
pub enum Value {
    /// Stored in the cell.
    Inline(Vec<u8>),
    /// Out of line.
    Out(Arc<[u8]>),
}

impl Value {
    /// The bytes of the value.
    pub fn bytes(&self) -> &[u8] {
        match self {
            Self::Inline(bytes) => bytes,
            Self::Out(bytes) => bytes,
        }
    }

    /// Whether the value is out of line.
    pub fn is_out(&self) -> bool {
        matches!(self, Self::Out(_))
    }
}

/// A short name for bytes, for messages: their length and a sum of them.
fn sum(bytes: &[u8]) -> String {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for &byte in bytes {
        hash = (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{} bytes #{:08x}", bytes.len(), hash as u32)
}

impl fmt::Debug for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Inline(bytes) => write!(f, "inline({})", sum(bytes)),
            Self::Out(bytes) => write!(f, "out({})", sum(bytes)),
        }
    }
}

/// A cell of a tree page.
#[derive(Clone, PartialEq, Eq)]
pub struct Cell {
    /// The key.
    pub key: Vec<u8>,
    /// The feature bytes.
    pub feature: Vec<u8>,
    /// The value hash.
    pub value_hash: Hash,
    /// The key-value hash.
    pub kv_hash: Hash,
    /// The value.
    pub value: Value,
}

impl fmt::Debug for Cell {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "cell(key {}, feature {}, hashes {:02x}../{:02x}.., {:?})",
            sum(&self.key),
            sum(&self.feature),
            self.value_hash[0],
            self.kv_hash[0],
            self.value
        )
    }
}

/// What a page says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Page {
    /// A tree page: its cells by slot.
    Tree(BTreeMap<u16, Cell>),
    /// A flat leaf: its entries by key.
    Flat(BTreeMap<Vec<u8>, Value>),
}

impl Page {
    /// How many values the page has out of line.
    pub fn values_out(&self) -> usize {
        match self {
            Self::Tree(cells) => cells.values().filter(|cell| cell.value.is_out()).count(),
            Self::Flat(entries) => entries.values().filter(|value| value.is_out()).count(),
        }
    }
}

/// The pages of a store: every page that is in use, by its id.
pub type Pages = BTreeMap<PageId, Arc<Page>>;

/// One change of a commit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Change {
    /// A new tree page with these cells. The store gives it a page id.
    NewTree {
        /// The cells, by slot.
        cells: Vec<(u16, Cell)>,
    },
    /// A new flat leaf with these entries, written as an image.
    NewFlat {
        /// The entries.
        entries: Vec<(Vec<u8>, Value)>,
    },
    /// A tree page gets a cell in a free slot.
    Insert {
        /// The page.
        page: PageId,
        /// The slot.
        slot: u16,
        /// The cell.
        cell: Cell,
    },
    /// A cell leaves its tree page.
    Remove {
        /// The page.
        page: PageId,
        /// The slot.
        slot: u16,
    },
    /// A hash of a cell changes: the key-value hash if `kv`, the value
    /// hash otherwise.
    Hash {
        /// The page.
        page: PageId,
        /// The slot.
        slot: u16,
        /// Which hash.
        kv: bool,
        /// The new hash.
        hash: Hash,
    },
    /// The feature bytes of a cell change.
    Feature {
        /// The page.
        page: PageId,
        /// The slot.
        slot: u16,
        /// The new bytes.
        feature: Vec<u8>,
    },
    /// The value of a cell is replaced.
    SetValue {
        /// The page.
        page: PageId,
        /// The slot.
        slot: u16,
        /// The new value.
        value: Value,
    },
    /// A flat leaf is written whole, as an image, with these entries. An
    /// entry with no value keeps the value it has under its key, which is
    /// out of line: the leaf goes on pointing to it where it lies.
    Rewrite {
        /// The page.
        page: PageId,
        /// The entries.
        entries: Vec<(Vec<u8>, Option<Value>)>,
    },
    /// A flat leaf gets an entry, or the value under a key it holds is
    /// replaced.
    Put {
        /// The page.
        page: PageId,
        /// The key.
        key: Vec<u8>,
        /// The value.
        value: Value,
    },
    /// An entry leaves its flat leaf.
    Delete {
        /// The page.
        page: PageId,
        /// The key.
        key: Vec<u8>,
    },
    /// The page is freed.
    Free {
        /// The page.
        page: PageId,
    },
}

impl Change {
    /// The page the change is made to, or `None` for one that makes a
    /// page.
    pub fn page(&self) -> Option<PageId> {
        match self {
            Self::NewTree { .. } | Self::NewFlat { .. } => None,
            Self::Insert { page, .. }
            | Self::Remove { page, .. }
            | Self::Hash { page, .. }
            | Self::Feature { page, .. }
            | Self::SetValue { page, .. }
            | Self::Rewrite { page, .. }
            | Self::Put { page, .. }
            | Self::Delete { page, .. }
            | Self::Free { page } => Some(*page),
        }
    }

    /// Whether the change makes a page.
    pub fn makes_page(&self) -> bool {
        self.page().is_none()
    }
}

/// A change that does not fit the pages it is made to. The workload makes
/// none, so this is a defect of the campaign itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Misfit(pub String);

impl fmt::Display for Misfit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "a change that does not fit the model: {}", self.0)
    }
}

impl std::error::Error for Misfit {}

fn misfit<T>(what: impl fmt::Display) -> Result<T, Misfit> {
    Err(Misfit(what.to_string()))
}

fn tree_of(pages: &mut Pages, page: PageId) -> Result<&mut BTreeMap<u16, Cell>, Misfit> {
    match pages.get_mut(&page).map(Arc::make_mut) {
        Some(Page::Tree(cells)) => Ok(cells),
        Some(Page::Flat(_)) => misfit(format!("page {page} is a flat leaf")),
        None => misfit(format!("page {page} is not in use")),
    }
}

fn flat_of(pages: &mut Pages, page: PageId) -> Result<&mut BTreeMap<Vec<u8>, Value>, Misfit> {
    match pages.get_mut(&page).map(Arc::make_mut) {
        Some(Page::Flat(entries)) => Ok(entries),
        Some(Page::Tree(_)) => misfit(format!("page {page} is a tree page")),
        None => misfit(format!("page {page} is not in use")),
    }
}

fn cell_of(pages: &mut Pages, page: PageId, slot: u16) -> Result<&mut Cell, Misfit> {
    let cells = tree_of(pages, page)?;
    match cells.get_mut(&slot) {
        Some(cell) => Ok(cell),
        None => misfit(format!("page {page} has no cell in slot {slot}")),
    }
}

/// What `pages` say once the commit of `changes` is made. `made` has the
/// id of each page a change makes, in the order of those changes.
pub fn apply(pages: &Pages, changes: &[Change], made: &[PageId]) -> Result<Pages, Misfit> {
    let mut pages = pages.clone();
    let mut made = made.iter().copied();
    let mut next = |pages: &Pages| match made.next() {
        Some(id) if pages.contains_key(&id) => misfit(format!("page {id} is in use already")),
        Some(id) => Ok(id),
        None => misfit("a page is made and has no id"),
    };
    for change in changes {
        match change {
            Change::NewTree { cells } => {
                let id = next(&pages)?;
                let cells: BTreeMap<u16, Cell> = cells.iter().cloned().collect();
                pages.insert(id, Arc::new(Page::Tree(cells)));
            }
            Change::NewFlat { entries } => {
                let id = next(&pages)?;
                let entries = entries.iter().cloned().collect();
                pages.insert(id, Arc::new(Page::Flat(entries)));
            }
            Change::Insert { page, slot, cell } => {
                let cells = tree_of(&mut pages, *page)?;
                if cells.insert(*slot, cell.clone()).is_some() {
                    return misfit(format!("slot {slot} of page {page} is taken"));
                }
            }
            Change::Remove { page, slot } => {
                if tree_of(&mut pages, *page)?.remove(slot).is_none() {
                    return misfit(format!("page {page} has no cell in slot {slot}"));
                }
            }
            Change::Hash {
                page,
                slot,
                kv,
                hash,
            } => {
                let cell = cell_of(&mut pages, *page, *slot)?;
                match kv {
                    true => cell.kv_hash = *hash,
                    false => cell.value_hash = *hash,
                }
            }
            Change::Feature {
                page,
                slot,
                feature,
            } => cell_of(&mut pages, *page, *slot)?.feature = feature.clone(),
            Change::SetValue { page, slot, value } => {
                cell_of(&mut pages, *page, *slot)?.value = value.clone();
            }
            Change::Rewrite { page, entries } => {
                let leaf = flat_of(&mut pages, *page)?;
                let mut written = BTreeMap::new();
                for (key, value) in entries {
                    let value = match (value, leaf.get(key)) {
                        (Some(value), _) => value.clone(),
                        (None, Some(kept)) if kept.is_out() => kept.clone(),
                        (None, _) => {
                            return misfit(format!("page {page} keeps no value out of line"))
                        }
                    };
                    written.insert(key.clone(), value);
                }
                *leaf = written;
            }
            Change::Put { page, key, value } => {
                flat_of(&mut pages, *page)?.insert(key.clone(), value.clone());
            }
            Change::Delete { page, key } => {
                if flat_of(&mut pages, *page)?.remove(key).is_none() {
                    return misfit(format!("page {page} has no entry under the key"));
                }
            }
            Change::Free { page } => {
                if pages.remove(page).is_none() {
                    return misfit(format!("page {page} is not in use"));
                }
            }
        }
    }
    if made.next().is_some() {
        return misfit("more ids than pages that are made");
    }
    Ok(pages)
}

/// What `pages` say once a reclaiming step has taken the cells in `slots`
/// out of the tree page `page`, or, with no slot, has freed the page.
pub fn reclaim(pages: &Pages, page: PageId, slots: &[u16]) -> Result<Pages, Misfit> {
    let mut pages = pages.clone();
    if slots.is_empty() {
        if pages.remove(&page).is_none() {
            return misfit(format!("page {page} is not in use"));
        }
        return Ok(pages);
    }
    let cells = tree_of(&mut pages, page)?;
    for slot in slots {
        if cells.remove(slot).is_none() {
            return misfit(format!("page {page} has no cell in slot {slot}"));
        }
    }
    Ok(pages)
}

/// The state of a store after one item of its log that changes what its
/// pages say: the group of a commit, or the record of a reclaiming step.
#[derive(Clone, Debug)]
pub struct Point {
    /// The version of the last commit at this point.
    pub version: u64,
    /// Where the item ends in the log. `None` for the store before its
    /// first item, and for an item whose append has not returned.
    pub end: Option<LogPlace>,
    /// What the pages say.
    pub pages: Arc<Pages>,
}

/// Which of the points of a history a recovered store may be at.
#[derive(Clone, Debug)]
pub struct Expect {
    /// Whether the store's first manifest is in place: a state with none
    /// is then no state of this store.
    pub created: bool,
    /// The first point a recovery may land on: every item up to it was
    /// reported durable.
    pub floor: usize,
    /// The last point whose item is in the log.
    pub latest: usize,
    /// An item whose append has begun and has not been reported as in the
    /// log or as out of it for good: a recovery may land on it as well.
    pub attempt: Option<Arc<Point>>,
}

impl Expect {
    /// What is expected of a store that was just made: nothing yet.
    pub fn of_new_store() -> Self {
        Self {
            created: false,
            floor: 0,
            latest: 0,
            attempt: None,
        }
    }

    /// What is expected of a state whose every item is durable and that is
    /// at the point `at`.
    pub fn exactly(at: usize) -> Self {
        Self {
            created: true,
            floor: at,
            latest: at,
            attempt: None,
        }
    }
}

/// Where a recovered store was found to be.
#[derive(Clone, Debug)]
pub enum Landing {
    /// The directory holds no store: its first manifest never became
    /// durable.
    NoStore,
    /// At this point of the history.
    Point(usize),
    /// At the item whose append was in doubt.
    Attempt(Arc<Point>),
}

impl Landing {
    /// Whether a state that was found here before may be found here under
    /// `expect`.
    pub fn allowed(&self, expect: &Expect) -> bool {
        match self {
            Self::NoStore => !expect.created,
            Self::Point(at) => (expect.floor..=expect.latest).contains(at),
            Self::Attempt(point) => expect
                .attempt
                .as_ref()
                .is_some_and(|attempt| Arc::ptr_eq(attempt, point)),
        }
    }
}

/// Why a recovered store is at no point it may be at.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Mismatch {
    /// It landed on a version below one that was reported durable.
    BelowFloor {
        /// The version it landed on.
        landed: u64,
        /// The version of the first point it may land on.
        floor: u64,
    },
    /// It landed on a version that no commit made.
    AboveLatest {
        /// The version it landed on.
        landed: u64,
        /// The version of the last point.
        latest: u64,
    },
    /// Its pages are those of no point of the version it landed on.
    Pages {
        /// The version it landed on.
        landed: u64,
        /// How its pages differ from those of the nearest point.
        difference: String,
    },
}

impl fmt::Display for Mismatch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BelowFloor { landed, floor } => write!(
                f,
                "landed on version {landed}, below version {floor}, which was reported durable"
            ),
            Self::AboveLatest { landed, latest } => write!(
                f,
                "landed on version {landed}, and the last commit made version {latest}"
            ),
            Self::Pages { landed, difference } => write!(
                f,
                "landed on version {landed} with pages that no commit left: {difference}"
            ),
        }
    }
}

/// How `found` differs from `expected`, in a few lines.
pub fn difference(found: &Pages, expected: &Pages) -> String {
    let mut lines = Vec::new();
    for (id, page) in expected {
        match found.get(id) {
            None => lines.push(format!("page {id} is missing")),
            Some(other) if other != page => {
                lines.push(format!("page {id} says {other:?}, not {page:?}"));
            }
            Some(_) => {}
        }
    }
    for id in found.keys().filter(|id| !expected.contains_key(id)) {
        lines.push(format!("page {id} is in use and should not be"));
    }
    const SHOWN: usize = 4;
    let more = lines.len().saturating_sub(SHOWN);
    lines.truncate(SHOWN);
    if more > 0 {
        lines.push(format!("and {more} more"));
    }
    lines.join("; ")
}

/// The points of one store, in the order of its log.
#[derive(Clone, Debug)]
pub struct History {
    points: Vec<Point>,
}

impl Default for History {
    fn default() -> Self {
        Self::new()
    }
}

impl History {
    /// The history of a store that was just made: one point, with no page.
    pub fn new() -> Self {
        Self {
            points: vec![Point {
                version: 0,
                end: None,
                pages: Arc::default(),
            }],
        }
    }

    /// The number of the last point.
    pub fn latest(&self) -> usize {
        self.points.len() - 1
    }

    /// The last point.
    pub fn last(&self) -> &Point {
        self.points.last().expect("a history has its first point")
    }

    /// The point `at`.
    pub fn point(&self, at: usize) -> &Point {
        &self.points[at]
    }

    /// How many points there are.
    pub fn len(&self) -> usize {
        self.points.len()
    }

    /// Whether the history has no point, which it never is.
    pub fn is_empty(&self) -> bool {
        self.points.is_empty()
    }

    /// Adds a point after the last.
    pub fn push(&mut self, point: Point) {
        self.points.push(point);
    }

    /// The history up to the point `at`, with nothing after it: what a
    /// store that a crash left at that point goes on from.
    pub fn up_to(&self, at: usize) -> Self {
        Self {
            points: self.points[..=at].to_vec(),
        }
    }

    /// The last point whose item ends at or before `durable`, the place the
    /// log is durable up to.
    pub fn floor(&self, durable: Option<LogPlace>) -> usize {
        let covered = |point: &Point| {
            point
                .end
                .is_none_or(|end| durable.is_some_and(|to| end <= to))
        };
        self.points
            .iter()
            .rposition(covered)
            .expect("the first point has no item")
    }

    /// Where a store is that landed on `landed` with `found` as its pages:
    /// a point of that version at or after the floor of `expect`, or the
    /// item in doubt.
    pub fn find(&self, expect: &Expect, landed: u64, found: &Pages) -> Result<Landing, Mismatch> {
        let attempt = expect
            .attempt
            .as_ref()
            .filter(|attempt| attempt.version == landed);
        if let Some(attempt) = attempt.filter(|attempt| *attempt.pages == *found) {
            return Ok(Landing::Attempt(attempt.clone()));
        }
        let allowed = &self.points[expect.floor..=expect.latest];
        let at = allowed
            .iter()
            .rposition(|point| point.version == landed && *point.pages == *found);
        if let Some(at) = at {
            return Ok(Landing::Point(expect.floor + at));
        }
        let floor = self.points[expect.floor].version;
        if landed < floor {
            return Err(Mismatch::BelowFloor { landed, floor });
        }
        let nearest = allowed
            .iter()
            .rev()
            .find(|point| point.version == landed)
            .map(|point| &point.pages)
            .or(attempt.map(|attempt| &attempt.pages));
        match nearest {
            Some(pages) => Err(Mismatch::Pages {
                landed,
                difference: difference(found, pages),
            }),
            None => Err(Mismatch::AboveLatest {
                landed,
                latest: self.points[expect.latest].version,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page(raw: u32) -> PageId {
        PageId::new(raw).expect("a page id")
    }

    fn cell(key: &[u8], value: Value) -> Cell {
        Cell {
            key: key.to_vec(),
            feature: vec![7],
            value_hash: [1; 32],
            kv_hash: [2; 32],
            value,
        }
    }

    fn out(bytes: &[u8]) -> Value {
        Value::Out(Arc::from(bytes))
    }

    fn place(offset: u64) -> LogPlace {
        let file = lignin_format::FileId::new(1).expect("a file id");
        LogPlace::new(file, 1000 + offset).expect("a place")
    }

    #[test]
    fn a_commit_makes_changes_and_frees_pages() {
        let made = apply(
            &Pages::new(),
            &[
                Change::NewTree {
                    cells: vec![(1, cell(b"a", Value::Inline(vec![1])))],
                },
                Change::NewFlat {
                    entries: vec![(b"k".to_vec(), out(b"long"))],
                },
            ],
            &[page(1), page(2)],
        )
        .expect("fits");
        assert_eq!(made.len(), 2);
        assert_eq!(made[&page(2)].values_out(), 1);

        let changed = apply(
            &made,
            &[
                Change::Insert {
                    page: page(1),
                    slot: 2,
                    cell: cell(b"b", out(b"other")),
                },
                Change::Hash {
                    page: page(1),
                    slot: 1,
                    kv: true,
                    hash: [9; 32],
                },
                Change::Hash {
                    page: page(1),
                    slot: 1,
                    kv: false,
                    hash: [8; 32],
                },
                Change::Feature {
                    page: page(1),
                    slot: 1,
                    feature: vec![4, 4],
                },
                Change::SetValue {
                    page: page(1),
                    slot: 1,
                    value: out(b"now out"),
                },
                Change::Put {
                    page: page(2),
                    key: b"m".to_vec(),
                    value: Value::Inline(vec![3]),
                },
                Change::Delete {
                    page: page(2),
                    key: b"k".to_vec(),
                },
            ],
            &[],
        )
        .expect("fits");
        let Page::Tree(cells) = &*changed[&page(1)] else {
            panic!("a tree page");
        };
        assert_eq!(cells[&1].kv_hash, [9; 32]);
        assert_eq!(cells[&1].value_hash, [8; 32]);
        assert_eq!(cells[&1].feature, [4, 4]);
        assert_eq!(cells[&1].value, out(b"now out"));
        assert_eq!(changed[&page(1)].values_out(), 2);
        assert_eq!(changed[&page(2)].values_out(), 0);
        // The pages before the commit are as they were.
        assert_eq!(made[&page(1)].values_out(), 0);

        let rewritten = apply(
            &changed,
            &[
                Change::Rewrite {
                    page: page(2),
                    entries: vec![
                        (b"m".to_vec(), Some(Value::Inline(vec![3]))),
                        (b"z".to_vec(), Some(out(b"whole"))),
                    ],
                },
                Change::Remove {
                    page: page(1),
                    slot: 2,
                },
            ],
            &[],
        )
        .expect("fits");
        assert_eq!(rewritten[&page(2)].values_out(), 1);
        // A leaf that is written whole again keeps a value out of line
        // under its key, and loses the entries that are not written.
        let keep = |key: &[u8]| Change::Rewrite {
            page: page(2),
            entries: vec![(key.to_vec(), None)],
        };
        let rewritten = apply(&rewritten, &[keep(b"z")], &[]).expect("fits");
        let Page::Flat(entries) = &*rewritten[&page(2)] else {
            panic!("a flat leaf");
        };
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[&b"z".to_vec()], out(b"whole"));
        // A value in line is written, not kept, and so is one that is not
        // there.
        let two = apply(
            &rewritten,
            &[Change::Put {
                page: page(2),
                key: b"m".to_vec(),
                value: Value::Inline(vec![3]),
            }],
            &[],
        )
        .expect("fits");
        assert!(apply(&two, &[keep(b"m")], &[]).is_err());
        assert!(apply(&two, &[keep(b"none")], &[]).is_err());
        let freed = apply(&rewritten, &[Change::Free { page: page(1) }], &[]).expect("fits");
        assert!(!freed.contains_key(&page(1)));
    }

    #[test]
    fn a_change_that_does_not_fit_is_refused() {
        let pages = apply(
            &Pages::new(),
            &[
                Change::NewTree {
                    cells: vec![(1, cell(b"a", Value::Inline(vec![1])))],
                },
                Change::NewFlat { entries: vec![] },
            ],
            &[page(1), page(2)],
        )
        .expect("fits");
        let tree_change = |slot| Change::Remove {
            page: page(1),
            slot,
        };
        let refused: Vec<(Vec<Change>, Vec<PageId>)> = vec![
            (vec![Change::NewTree { cells: vec![] }], vec![]),
            (vec![Change::NewTree { cells: vec![] }], vec![page(1)]),
            (vec![], vec![page(5)]),
            (vec![tree_change(4)], vec![]),
            (
                vec![Change::Insert {
                    page: page(1),
                    slot: 1,
                    cell: cell(b"b", Value::Inline(vec![])),
                }],
                vec![],
            ),
            (
                vec![Change::Remove {
                    page: page(2),
                    slot: 1,
                }],
                vec![],
            ),
            (
                vec![Change::Remove {
                    page: page(3),
                    slot: 1,
                }],
                vec![],
            ),
            (
                vec![Change::Hash {
                    page: page(1),
                    slot: 9,
                    kv: false,
                    hash: [0; 32],
                }],
                vec![],
            ),
            (
                vec![Change::Delete {
                    page: page(2),
                    key: b"none".to_vec(),
                }],
                vec![],
            ),
            (
                vec![Change::Put {
                    page: page(1),
                    key: vec![],
                    value: Value::Inline(vec![]),
                }],
                vec![],
            ),
            (
                vec![Change::Put {
                    page: page(7),
                    key: vec![],
                    value: Value::Inline(vec![]),
                }],
                vec![],
            ),
            (vec![Change::Free { page: page(9) }], vec![]),
        ];
        for (changes, made) in refused {
            let error = apply(&pages, &changes, &made).expect_err("does not fit");
            assert!(error.to_string().contains("does not fit"), "{error}");
        }
        assert!(reclaim(&pages, page(9), &[]).is_err());
        assert!(reclaim(&pages, page(1), &[3]).is_err());
        assert!(reclaim(&pages, page(2), &[1]).is_err());
    }

    #[test]
    fn a_reclaiming_step_takes_cells_out_or_frees_the_page() {
        let pages = apply(
            &Pages::new(),
            &[Change::NewTree {
                cells: vec![
                    (1, cell(b"a", Value::Inline(vec![1]))),
                    (2, cell(b"b", Value::Inline(vec![2]))),
                ],
            }],
            &[page(4)],
        )
        .expect("fits");
        let fewer = reclaim(&pages, page(4), &[2]).expect("fits");
        assert_eq!(
            fewer[&page(4)],
            Arc::new(Page::Tree([(1, cell(b"a", Value::Inline(vec![1])))].into()))
        );
        assert!(reclaim(&fewer, page(4), &[]).expect("fits").is_empty());
    }

    /// A history of three commits and a reclaiming step after the second.
    fn history() -> History {
        let mut history = History::new();
        let mut pages = Pages::new();
        for (version, end) in [(1, 10), (2, 20)] {
            let made = [Change::NewTree {
                cells: vec![(1, cell(&[version as u8], Value::Inline(vec![])))],
            }];
            pages = apply(&pages, &made, &[page(version as u32)]).expect("fits");
            history.push(Point {
                version,
                end: Some(place(end)),
                pages: Arc::new(pages.clone()),
            });
        }
        pages = reclaim(&pages, page(1), &[]).expect("fits");
        history.push(Point {
            version: 2,
            end: Some(place(30)),
            pages: Arc::new(pages.clone()),
        });
        pages = apply(&pages, &[Change::Free { page: page(2) }], &[]).expect("fits");
        history.push(Point {
            version: 3,
            end: Some(place(40)),
            pages: Arc::new(pages),
        });
        history
    }

    #[test]
    fn the_floor_is_the_last_point_the_log_is_durable_through() {
        let history = history();
        assert_eq!((history.len(), history.latest()), (5, 4));
        assert!(!history.is_empty());
        assert_eq!(history.floor(None), 0);
        assert_eq!(history.floor(Some(place(5))), 0);
        assert_eq!(history.floor(Some(place(10))), 1);
        assert_eq!(history.floor(Some(place(29))), 2);
        assert_eq!(history.floor(Some(place(30))), 3);
        assert_eq!(history.floor(Some(place(99))), 4);
        assert_eq!(history.up_to(2).latest(), 2);
        assert_eq!(History::default().last().version, 0);
    }

    #[test]
    fn a_store_is_found_at_a_point_of_its_version_at_or_after_the_floor() {
        let history = history();
        let expect = Expect {
            created: true,
            floor: 1,
            latest: 4,
            attempt: None,
        };
        let pages = |at: usize| history.point(at).pages.clone();
        for at in 1..=4 {
            let version = history.point(at).version;
            let found = history.find(&expect, version, &pages(at)).expect("found");
            assert!(
                matches!(found, Landing::Point(point) if point == at),
                "{found:?}"
            );
            assert!(found.allowed(&expect));
        }
        // The same version, before and after the reclaiming step.
        assert_ne!(pages(2), pages(3));

        // Below the floor, above the latest, and pages of another point.
        assert!(matches!(
            history.find(&expect, 0, &Pages::new()),
            Err(Mismatch::BelowFloor {
                landed: 0,
                floor: 1
            })
        ));
        assert!(matches!(
            history.find(&expect, 4, &pages(4)),
            Err(Mismatch::AboveLatest {
                landed: 4,
                latest: 3
            })
        ));
        let wrong = history
            .find(&expect, 3, &pages(1))
            .expect_err("other pages");
        assert!(
            matches!(wrong, Mismatch::Pages { landed: 3, .. }),
            "{wrong}"
        );
        for mismatch in [
            Mismatch::BelowFloor {
                landed: 0,
                floor: 1,
            },
            Mismatch::AboveLatest {
                landed: 4,
                latest: 3,
            },
            wrong,
        ] {
            assert!(mismatch.to_string().contains("landed on version"));
        }

        // A point before the floor is none the store may be at, although
        // its version is the floor's.
        let after_step = Expect {
            floor: 3,
            ..expect.clone()
        };
        assert!(history.find(&after_step, 2, &pages(2)).is_err());
        assert!(history.find(&after_step, 2, &pages(3)).is_ok());
        assert!(!Landing::Point(2).allowed(&after_step));
    }

    #[test]
    fn an_item_in_doubt_may_be_landed_on_and_only_while_it_is_in_doubt() {
        let history = history();
        let other = apply(
            &history.last().pages,
            &[Change::NewFlat { entries: vec![] }],
            &[page(8)],
        )
        .expect("fits");
        let attempt = Arc::new(Point {
            version: 4,
            end: None,
            pages: Arc::new(other.clone()),
        });
        let expect = Expect {
            created: true,
            floor: 4,
            latest: 4,
            attempt: Some(attempt.clone()),
        };
        let found = history.find(&expect, 4, &other).expect("the attempt");
        assert!(matches!(&found, Landing::Attempt(point) if Arc::ptr_eq(point, &attempt)));
        assert!(found.allowed(&expect));
        assert!(!found.allowed(&Expect::exactly(4)));
        // With other pages, the difference is to the attempt's.
        let wrong = history
            .find(&expect, 4, &Pages::new())
            .expect_err("other pages");
        assert!(matches!(wrong, Mismatch::Pages { landed: 4, .. }));
        // Without the attempt, nobody made version 4.
        assert!(matches!(
            history.find(&Expect::exactly(4), 4, &other),
            Err(Mismatch::AboveLatest { .. })
        ));

        assert!(Landing::NoStore.allowed(&Expect::of_new_store()));
        assert!(!Landing::NoStore.allowed(&expect));
    }

    #[test]
    fn a_difference_names_the_pages() {
        let history = history();
        let (two, last) = (&history.point(2).pages, &history.last().pages);
        let said = difference(last, two);
        assert!(said.contains("page 1 is missing") && said.contains("page 2 is missing"));
        assert!(difference(two, last).contains("is in use and should not be"));
        let mut changed = (**two).clone();
        changed.insert(page(1), Arc::new(Page::Flat(BTreeMap::new())));
        assert!(difference(&changed, two).contains("page 1 says"));
        // Many differences are cut short.
        let many: Pages = (1..=9)
            .map(|raw| (page(raw), Arc::new(Page::Flat(BTreeMap::new()))))
            .collect();
        assert!(difference(&many, &Pages::new()).ends_with("and 5 more"));
        assert_eq!(format!("{:?}", out(b"ab")), "out(2 bytes #b545986a)");
        assert!(format!("{:?}", Value::Inline(vec![1])).starts_with("inline(1 bytes"));
        assert!(format!("{:?}", cell(b"a", out(b"x"))).starts_with("cell(key 1 bytes"));
    }
}
