//! The lazy range delete of a flat keyspace: unlinking a range without
//! reading it, the record of what it left, and the freeing of that.

use std::collections::HashSet;

use lignin_format::{
    flat_page::{FlatPage, FlatValue},
    value_log::ValuePointer,
    CommitVersion, PageId,
};

use lignin_store::values::ValueStoreError;

use super::{
    error::DropError,
    merk::Progress,
    step::{
        bucket_root, damaged, flat_page, place_of, seen, sort_released, value_bytes, Busy,
        EntryRemoved, Reclaimer, Step, LOOKS,
    },
};
use crate::{
    flat::{Committed, FlatConfig, FlatPages, FlatPagesMut, FlatTree, StepLog, Unlinked},
    OperationGuard,
};

const HEADER: u8 = 1;
const RECORD: u8 = 2;
const HEADER_KEY: [u8; 1] = [HEADER];
const ID_LEN: usize = 8;
const VERSION_LEN: usize = 8;
const PAGE_LEN: usize = PageId::ENCODED_LEN + 2;

/// What one lazy range delete left: the pages it took out of their tree
/// without reading them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlatDrop {
    /// The number of the record: no other record of its keyspace has had
    /// it or will.
    pub id: u64,
    /// The version the writer of the range delete read at. A page it left
    /// is as that version has it, or as the writer's own commit made it.
    pub at: CommitVersion,
    /// The pages, each with every page below it.
    pub pages: Vec<Unlinked>,
}

impl FlatDrop {
    /// The newest version a page that the range delete left can have been
    /// born at: the version after the one its writer read at, which is the
    /// version its commit took. A commit takes the version after its base
    /// (`transactions.md` CM3, steps 4 and 6), and a transaction that began
    /// over an older base has made its pages again over that one by then,
    /// this record among them (WT7). A page under one of the ids that was
    /// born later is another page, which has the id since the one that was
    /// left was freed.
    fn newest_birth(&self) -> CommitVersion {
        self.at.next().unwrap_or(self.at)
    }

    fn key(id: u64) -> [u8; 1 + ID_LEN] {
        let mut key = [RECORD; 1 + ID_LEN];
        key[1..].copy_from_slice(&id.to_be_bytes());
        key
    }

    fn encode(at: CommitVersion, pages: &[Unlinked]) -> Vec<u8> {
        let mut out = Vec::with_capacity(VERSION_LEN + pages.len() * PAGE_LEN);
        out.extend_from_slice(&at.get().to_le_bytes());
        for left in pages {
            out.extend_from_slice(&left.page.encode());
            match left.above {
                Some(level) => out.extend_from_slice(&[1, level]),
                None => out.extend_from_slice(&[0, 0]),
            }
        }
        out
    }

    fn decode(key: &[u8], value: &[u8]) -> Result<Self, DropError> {
        let id: [u8; ID_LEN] = key
            .try_into()
            .map_err(|_| DropError::Record("a key that is no number"))?;
        let (at, pages) = value
            .split_first_chunk::<VERSION_LEN>()
            .ok_or(DropError::Record("a record with no version"))?;
        let at = CommitVersion::new(u64::from_le_bytes(*at))
            .ok_or(DropError::Record("a version that is none"))?;
        let (pages, rest) = pages.as_chunks::<PAGE_LEN>();
        if !rest.is_empty() || pages.is_empty() {
            return Err(DropError::Record("pages that are not whole"));
        }
        let pages = pages
            .iter()
            .map(|&[a, b, c, d, known, level]| {
                let page =
                    PageId::decode([a, b, c, d]).ok_or(DropError::Record("a page that is none"))?;
                let above = match (known, level) {
                    (0, 0) => None,
                    (1, level) => Some(level),
                    _ => return Err(DropError::Record("a level that is none")),
                };
                Ok(Unlinked { page, above })
            })
            .collect::<Result<_, _>>()?;
        Ok(Self {
            id: u64::from_be_bytes(id),
            at,
            pages,
        })
    }
}

/// The records of the lazy range deletes of a store: one flat keyspace,
/// which the store names by its root page as it names its directory.
///
/// A range delete that unlinks ([`Self::unlink_range`]) is a write of its
/// transaction like any range delete, and takes effect at its commit
/// (`transactions.md` CV9, ST4). What it writes does not depend on how
/// many records the range holds: the leaf at each end of the range, the
/// branches above them, and one record here. The pages between are freed
/// later, by [`Reclaimer::reclaim_flat`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FlatDrops {
    tree: FlatTree,
}

impl FlatDrops {
    /// Makes the keyspace of the records, with none.
    pub fn create<P: FlatPagesMut + ?Sized>(
        pages: &mut P,
        config: FlatConfig,
    ) -> Result<Self, DropError> {
        let tree = FlatTree::create(pages, config)?;
        tree.put(pages, &HEADER_KEY, &1u64.to_le_bytes())?;
        Ok(Self { tree })
    }

    /// The keyspace of the records whose root is the page `root`.
    pub const fn open(root: PageId, config: FlatConfig) -> Self {
        Self {
            tree: FlatTree::open(root, config),
        }
    }

    /// The root page of the keyspace: what a store keeps to find the
    /// records again. It is the same page for good.
    pub const fn root_page(&self) -> PageId {
        self.tree.root()
    }

    /// The keyspace the records are in.
    pub const fn tree(&self) -> &FlatTree {
        &self.tree
    }

    /// Removes every record of `tree` whose key is at or after `start` and
    /// before `end`, or to the end of the keyspace with no `end`, without
    /// reading the pages whose whole key range lies inside the range
    /// ([`FlatTree::unlink_range`]), and records those pages here.
    ///
    /// `at` is the version the writer of `pages` reads at: the base of its
    /// transaction, which is the latest version when the transaction
    /// commits (`transactions.md` CM3). It is never a later one. With a
    /// later one, a page that takes the id of a freed one could be taken
    /// for it. And it is not an earlier one than the transaction commits
    /// over: a transaction that makes its pages again over another base
    /// (WT7) makes this call again, with that base. With an earlier one,
    /// the pages that the transaction wrote itself would be taken for
    /// pages of a later commit, and would never be freed.
    ///
    /// Returns the number of the record, or `None` if the range delete took
    /// no page out whole: then nothing is left to free.
    pub fn unlink_range<P: FlatPagesMut + ?Sized>(
        &self,
        pages: &mut P,
        tree: &FlatTree,
        start: &[u8],
        end: Option<&[u8]>,
        at: CommitVersion,
    ) -> Result<Option<u64>, DropError> {
        let left = tree.unlink_range(pages, start, end)?;
        if left.is_empty() {
            return Ok(None);
        }
        let id = self.next_id(pages)?;
        let next = id.checked_add(1).ok_or(DropError::IdsExhausted)?;
        self.tree.put(pages, &HEADER_KEY, &next.to_le_bytes())?;
        self.tree
            .put(pages, &FlatDrop::key(id), &FlatDrop::encode(at, &left))?;
        Ok(Some(id))
    }

    /// Removes every record of `tree` whose key starts with `prefix`, as
    /// [`Self::unlink_range`] removes a range: the lazy drop of one
    /// keyspace among several that share a tree.
    pub fn unlink_prefix<P: FlatPagesMut + ?Sized>(
        &self,
        pages: &mut P,
        tree: &FlatTree,
        prefix: &[u8],
        at: CommitVersion,
    ) -> Result<Option<u64>, DropError> {
        let mut end = prefix.to_vec();
        while end.last() == Some(&u8::MAX) {
            end.pop();
        }
        if let Some(last) = end.last_mut() {
            *last += 1;
        }
        let end = (!end.is_empty()).then_some(&end[..]);
        self.unlink_range(pages, tree, prefix, end, at)
    }

    /// The number the next record gets.
    pub fn next_id<P: FlatPages + ?Sized>(&self, pages: &P) -> Result<u64, DropError> {
        let mut header = Vec::new();
        let found = self.tree.get(pages, &HEADER_KEY, &mut header)?;
        let next = header.try_into().ok().map(u64::from_le_bytes);
        next.filter(|_| found)
            .ok_or(DropError::Record("a keyspace with no header"))
    }

    /// What every lazy range delete left that is not freed yet, in the
    /// order they were made.
    pub fn pending<P: FlatPages + ?Sized>(&self, pages: &P) -> Result<Vec<FlatDrop>, DropError> {
        let mut found = Vec::new();
        let mut cursor = self.tree.cursor_within(&[RECORD]);
        cursor.seek_to_first(pages)?;
        while cursor.valid() {
            let mut value = Vec::new();
            // A record that a page step took out under the walk is passed.
            if cursor.value(pages, &mut value)? {
                let key = cursor.key().unwrap_or_default();
                found.push(FlatDrop::decode(key, &value)?);
            }
            cursor.next(pages)?;
        }
        Ok(found)
    }
}

/// What freeing the pages of lazy range deletes has done so far.
#[derive(Debug, Default)]
pub struct FlatReclaimed {
    /// The pages freed.
    pub pages_freed: u64,
    /// How many of them were given no free link: no segment ever held the
    /// page under its id's present life, and the newest link of the id was
    /// the free link of an earlier page ([`Reclaimed::pages_never_linked`]).
    ///
    /// [`Reclaimed::pages_never_linked`]: super::Reclaimed::pages_never_linked
    pub pages_never_linked: u64,
    /// The value pointers that the entries of those pages held, which the
    /// value log was told of in the step that freed each page.
    pub values: u64,
    /// The bytes of the values they pointed at.
    pub value_bytes: u64,
    /// The records taken out.
    pub records: u64,
    /// The first count of live values that did not take the pointers of a
    /// step as they were reported, if one did not. The step was made all
    /// the same.
    pub miscount: Option<ValueStoreError>,
}

impl FlatReclaimed {
    fn miscounted(&mut self, miscount: Option<ValueStoreError>) {
        if self.miscount.is_none() {
            self.miscount = miscount;
        }
    }
}

/// One page that a range delete left, as the walk meets it.
enum Met {
    /// The id names no page any more, or another one: the page was freed.
    Gone,
    /// The page was freed by this call.
    Freed,
    /// The pages it names, which go first.
    Above(Vec<Unlinked>),
    Refused(Busy),
}

impl<'s, L: StepLog> Reclaimer<'s, L> {
    /// Frees the pages that the lazy range delete of `drop` left, by at
    /// most `steps` page steps, and then takes its record out of `drops`.
    /// Each leaf is read as it is freed: the value pointers its entries
    /// hold are released in that step, and the trees of its buckets of
    /// long keys are freed before it.
    ///
    /// A page goes after every page below it, and the record after every
    /// page. So the walk can end anywhere, by an error, a restart or a
    /// crash, and begin again from the record: a page that is no page any
    /// more, or that was born after the range delete can have committed,
    /// was freed, and is passed. Nothing but this reclaimer frees such a
    /// page, and no commit writes one, so the version tells.
    ///
    /// The [`Wait`](super::Wait) for the range delete is over, and `guard`
    /// is the guard of the caller's operation. An error other than
    /// [`DropError::Broken`] says that the step it came from changed
    /// nothing. After [`DropError::Broken`] the caller stops: the store is
    /// not as its log has it, and a call that began again would meet the
    /// page of that step half freed.
    ///
    /// Every call begins at the record and asks of each page below a
    /// branch whether it is still there, so a call is given steps by the
    /// hundred, not one at a time.
    pub fn reclaim_flat<G: OperationGuard + ?Sized>(
        &self,
        drops: &FlatDrops,
        drop: &FlatDrop,
        guard: &G,
        steps: usize,
        done: &mut FlatReclaimed,
    ) -> Result<Progress, DropError> {
        let config = drops.tree.config();
        let mut left = steps;
        // A page stays here until every page below it is gone.
        let mut walk: Vec<Unlinked> = drop.pages.iter().rev().copied().collect();
        let mut on_way = HashSet::with_capacity(walk.len());
        let twice = "a page that the pages a range delete left name twice";
        if let Some(left) = walk.iter().find(|left| !on_way.insert(left.page)) {
            return Err(damaged(left.page, twice));
        }
        while let Some(&top) = walk.last() {
            if left == 0 {
                return Ok(Progress::More);
            }
            match self.meet(drop, &config, top, done)? {
                Met::Gone => drop_top(&mut walk, &mut on_way),
                Met::Freed => {
                    left -= 1;
                    drop_top(&mut walk, &mut on_way);
                }
                Met::Above(below) => {
                    // No page is below itself, and none is named twice.
                    if let Some(left) = below.iter().find(|left| !on_way.insert(left.page)) {
                        return Err(damaged(left.page, twice));
                    }
                    walk.extend(below);
                }
                Met::Refused(busy) => return Ok(Progress::Blocked(busy)),
            }
        }
        if left == 0 {
            return Ok(Progress::More);
        }
        // The record has a number that no other has, so one that is not as
        // it was read is the same record, with its value in another place:
        // it is read again.
        let key = FlatDrop::key(drop.id);
        for _ in 0..LOOKS {
            let pages = Committed::new(self.loader, self.values, guard, self.log.latest());
            let Some(seen) = seen(&pages, &drops.tree, &key)? else {
                return Ok(Progress::Done);
            };
            match self.remove_entry(&drops.tree, &key, &seen)? {
                Step::Refused(busy) => return Ok(Progress::Blocked(busy)),
                Step::Made(EntryRemoved::Removed(miscount)) => {
                    done.records += 1;
                    done.miscounted(miscount);
                    return Ok(Progress::Done);
                }
                Step::Made(EntryRemoved::Absent) => return Ok(Progress::Done),
                Step::Made(EntryRemoved::Changed) => {}
            }
        }
        Ok(Progress::Blocked(Busy::Contended))
    }

    /// Looks at one page that the range delete left: names what is below
    /// it and still a page, or frees it.
    fn meet(
        &self,
        drop: &FlatDrop,
        config: &FlatConfig,
        left: Unlinked,
        done: &mut FlatReclaimed,
    ) -> Result<Met, DropError> {
        let page = left.page;
        for _ in 0..LOOKS {
            let Some(frame) = self.current(page)? else {
                return Ok(Met::Gone);
            };
            if frame.born() > drop.newest_birth() {
                return Ok(Met::Gone);
            }
            let mut below = Vec::new();
            let mut released: Vec<ValuePointer> = Vec::new();
            match flat_page(&frame, page, left.above)? {
                FlatPage::Branch(branch) => {
                    let above = Some(branch.level());
                    for index in 0..=branch.len() {
                        let child = branch.child(index);
                        let page = child.ok_or(damaged(page, "a branch that names no child"))?;
                        below.push(Unlinked { page, above });
                    }
                }
                FlatPage::Leaf(leaf) => {
                    for (key, value) in leaf.iter() {
                        if key.len() > config.key_limit() {
                            let root = bucket_root(page, value)?;
                            below.push(Unlinked {
                                page: root,
                                above: None,
                            });
                        } else if let FlatValue::OutOfLine(place) = value {
                            released.extend(place_of(page, place)?.pointer());
                        }
                    }
                }
            }
            let mut still = Vec::with_capacity(below.len());
            for child in below {
                if self.is_left(drop, child.page)? {
                    still.push(child);
                }
            }
            if !still.is_empty() {
                return Ok(Met::Above(still));
            }
            sort_released(page, &mut released)?;
            match self.free_unlinked(&frame, &released)? {
                None => continue,
                Some(Step::Refused(busy)) => return Ok(Met::Refused(busy)),
                Some(Step::Made(freed)) => {
                    let miscount = freed.miscount;
                    done.pages_freed += 1;
                    done.pages_never_linked += u64::from(freed.never_linked);
                    done.values += released.len() as u64;
                    done.value_bytes += value_bytes(&released);
                    done.miscounted(miscount);
                    return Ok(Met::Freed);
                }
            }
        }
        Ok(Met::Refused(Busy::Contended))
    }

    /// Whether the id `page` still names the page that the range delete of
    /// `drop` left under it.
    fn is_left(&self, drop: &FlatDrop, page: PageId) -> Result<bool, DropError> {
        let frame = self.current(page)?;
        Ok(frame.is_some_and(|frame| frame.born() <= drop.newest_birth()))
    }

    /// What every lazy range delete left that is not freed yet, as the
    /// latest version has it.
    pub fn flat_drops<G: OperationGuard + ?Sized>(
        &self,
        drops: &FlatDrops,
        guard: &G,
    ) -> Result<Vec<FlatDrop>, DropError> {
        let pages = Committed::new(self.loader, self.values, guard, self.log.latest());
        drops.pending(&pages)
    }
}

fn drop_top(walk: &mut Vec<Unlinked>, on_way: &mut HashSet<PageId>) {
    if let Some(top) = walk.pop() {
        on_way.remove(&top.page);
    }
}
