mod errors;
mod grove;
mod property;
mod rules;
mod values;

use std::collections::{BTreeMap, HashMap};

use lignin_format::{
    delta::Delta,
    tree_page::{CellValue, TreePageMut},
    value_log::{PendingValue, ValuePlace, ValuePointer},
    CellAddress, FileId, PageId, PageSize, SlotId,
};
use lignin_store::load::FRAME_FORMAT;

use super::*;
use crate::node::{ChangeSink, Lease, NodeError, NodePage, NodePageMut};

const SIZE: PageSize = PageSize::MIN;

fn page(raw: u32) -> PageId {
    PageId::new(raw).expect("not zero")
}

fn slot(raw: u16) -> SlotId {
    SlotId::new(raw).expect("not zero")
}

fn address(page_raw: u32, slot_raw: u16) -> CellAddress {
    CellAddress::new(page(page_raw), slot(slot_raw))
}

/// SplitMix64.
#[derive(Debug, Clone)]
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, bound: usize) -> usize {
        (self.next() % bound as u64) as usize
    }

    fn chance(&mut self, percent: usize) -> bool {
        self.below(100) < percent
    }
}

/// What one record did, for the tests that count records.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Rec {
    Created,
    Insert,
    Remove,
    Hash,
    Child,
    Link,
    Mark,
    Hosted,
    Feature,
    Value,
    Replace,
}

/// The sink of the tests. It makes every record it is told on a copy of the
/// page, so the copies are what the records alone give.
#[derive(Debug)]
struct Shadow {
    size: PageSize,
    frames: BTreeMap<PageId, Vec<u8>>,
    records: Vec<(PageId, Rec)>,
}

impl ChangeSink for Shadow {
    fn created(&mut self, page: PageId, departures: u64) {
        let mut frame = vec![0; self.size.get()];
        TreePageMut::init(&mut frame, self.size, page, departures).expect("a frame of a page");
        assert!(
            self.frames.insert(page, frame).is_none(),
            "{page} is started once"
        );
        self.records.push((page, Rec::Created));
    }

    fn changed(&mut self, page: PageId, change: &Delta<'_>) {
        let frame = self
            .frames
            .get_mut(&page)
            .expect("a page that changes was started");
        let mut copy = TreePageMut::open(frame, self.size, FRAME_FORMAT).expect("a tree page");
        change
            .apply(&mut copy)
            .expect("a record that a page took is one its copy takes");
        let rec = match change {
            Delta::Insert { cell, .. } => {
                assert!(
                    cell.value_pointer().is_none(),
                    "a cell arrives with its value in it or pending, never with a pointer"
                );
                Rec::Insert
            }
            Delta::Replace { .. } => Rec::Replace,
            Delta::Remove { .. } => Rec::Remove,
            Delta::Hash { .. } => Rec::Hash,
            Delta::Child { .. } => Rec::Child,
            Delta::Link { .. } => Rec::Link,
            Delta::FragmentRoot { .. } => Rec::Mark,
            Delta::HostedRoot { .. } | Delta::HostedRoots { .. } => Rec::Hosted,
            Delta::Feature { .. } => Rec::Feature,
            Delta::Value { .. } => Rec::Value,
        };
        self.records.push((page, rec));
    }
}

/// Tree pages in memory, as one write scope has them.
#[derive(Debug)]
struct Mem {
    size: PageSize,
    lease: Lease,
    frames: BTreeMap<PageId, Vec<u8>>,
    shadow: Shadow,
    next: u32,
    freed: Vec<PageId>,
    /// The pages that were read or changed, in order.
    touched: Vec<PageId>,
    /// How many more changes the pages take before they fail.
    writes_left: Option<usize>,
    /// The hosts whose element is marked pending.
    behind: Vec<CellAddress>,
}

impl Mem {
    fn new(size: PageSize) -> Self {
        Self {
            size,
            lease: Lease::take(),
            frames: BTreeMap::new(),
            shadow: Shadow {
                size,
                frames: BTreeMap::new(),
                records: Vec::new(),
            },
            next: 1,
            freed: Vec::new(),
            touched: Vec::new(),
            writes_left: None,
            behind: Vec::new(),
        }
    }

    fn view(&self, page: PageId) -> NodePage<'_> {
        NodePage::open(&self.frames[&page], self.lease).expect("a tree page")
    }

    fn used(&self, page: PageId) -> usize {
        pages::used_bytes(&self.view(page))
    }

    fn cells(&self) -> usize {
        self.frames.keys().map(|page| self.view(*page).len()).sum()
    }

    /// How many records of `kind` the sink was told since `from`.
    fn count(&self, from: usize, kind: Rec) -> usize {
        let records = &self.shadow.records[from..];
        records.iter().filter(|(_, rec)| *rec == kind).count()
    }

    fn missing(page: PageId) -> FragmentError {
        FragmentError::Pages(format!("no {page}").into())
    }
}

impl Pages for Mem {
    fn read<R>(
        &mut self,
        page: PageId,
        read: impl FnOnce(NodePage<'_>) -> R,
    ) -> Result<R, FragmentError> {
        self.touched.push(page);
        let frame = self.frames.get(&page).ok_or_else(|| Self::missing(page))?;
        Ok(read(NodePage::open(frame, self.lease)?))
    }

    fn write<R>(
        &mut self,
        page: PageId,
        change: impl FnOnce(&mut PageMut<'_, '_>) -> Result<R, NodeError>,
    ) -> Result<R, FragmentError> {
        self.touched.push(page);
        if let Some(left) = &mut self.writes_left {
            *left = left
                .checked_sub(1)
                .ok_or_else(|| FragmentError::Pages("the pages are closed".into()))?;
        }
        let frame = self
            .frames
            .get_mut(&page)
            .ok_or_else(|| Self::missing(page))?;
        let sink: &mut dyn ChangeSink = &mut self.shadow;
        let mut page = NodePageMut::open(frame, self.size, self.lease, sink)?;
        Ok(change(&mut page)?)
    }

    fn create(&mut self) -> Result<PageId, FragmentError> {
        let id = page(self.next);
        self.next += 1;
        let mut frame = vec![0; self.size.get()];
        let sink: &mut dyn ChangeSink = &mut self.shadow;
        NodePageMut::create(&mut frame, self.size, id, self.lease, sink)?;
        self.frames.insert(id, frame);
        Ok(id)
    }

    fn host_is_behind(&mut self, host: CellAddress) -> bool {
        self.behind.contains(&host)
    }

    fn free(&mut self, page: PageId) -> Result<(), FragmentError> {
        let frame = self
            .frames
            .remove(&page)
            .ok_or_else(|| Self::missing(page))?;
        assert!(
            NodePage::open(&frame, self.lease)?.is_empty(),
            "only an empty page is freed"
        );
        self.shadow.frames.remove(&page);
        self.freed.push(page);
        Ok(())
    }
}

/// A value that is in the value log.
#[derive(Debug, Clone)]
struct Logged {
    owner: PageId,
    bytes: Vec<u8>,
    live: bool,
}

/// The values of the tests: a value log that records the owner of each
/// value, and the values a commit has not put there yet.
#[derive(Debug, Default)]
struct Log {
    logged: HashMap<ValuePointer, Logged>,
    /// The pending values by their id: the bytes, and the page that owns
    /// them once a cell holds them.
    pending: HashMap<[u8; 8], (Vec<u8>, Option<PageId>)>,
    next: u64,
    /// The places that were reported as carried: the place, the page it
    /// left and the page it went to.
    carried: Vec<(ValuePlace, PageId, PageId)>,
    released: Vec<(ValuePlace, PageId)>,
    appended_again: usize,
}

impl Log {
    /// A value that the next write gives to a cell out of line.
    fn pending(&mut self, bytes: &[u8]) -> PendingValue {
        self.next += 1;
        let id = self.next.to_le_bytes();
        self.pending.insert(id, (bytes.to_vec(), None));
        PendingValue::new(id, bytes.len() as u32).expect("a pending value")
    }

    fn drop_place(&mut self, place: ValuePlace, page: PageId) -> Vec<u8> {
        match place {
            ValuePlace::Placed(pointer) => {
                let logged = self
                    .logged
                    .get_mut(&pointer)
                    .expect("the pointer is in the log");
                assert_eq!(logged.owner, page, "the page that held the pointer owns it");
                assert!(logged.live, "a pointer is released once");
                logged.live = false;
                logged.bytes.clone()
            }
            ValuePlace::Pending(pending) => {
                let (bytes, owner) = self
                    .pending
                    .remove(&pending.id())
                    .expect("the pending value is known");
                assert_eq!(owner, Some(page), "the page that held the value owns it");
                bytes
            }
        }
    }

    /// The bytes of the value a cell of `page` holds.
    fn read(&self, value: CellValue<'_>, page: PageId) -> Vec<u8> {
        match value {
            CellValue::Inline(bytes) => bytes.to_vec(),
            CellValue::OutOfLine(pointer) => {
                let logged = &self.logged[&pointer];
                assert!(logged.live, "a cell holds a live value");
                assert_eq!(
                    logged.owner, page,
                    "the value's record names the cell's page"
                );
                logged.bytes.clone()
            }
            CellValue::Pending(pending) => {
                let (bytes, owner) = &self.pending[&pending.id()];
                assert_eq!(
                    *owner,
                    Some(page),
                    "the pending value is owned by the cell's page"
                );
                bytes.clone()
            }
        }
    }

    /// Write-back: every pending value of the pages is appended to the
    /// value log with its page as the owner, and its pointer is put into
    /// its cell.
    fn write_back(&mut self, mem: &mut Mem) {
        let pages: Vec<PageId> = mem.frames.keys().copied().collect();
        for id in pages {
            let pending: Vec<_> = mem.view(id).tree_page().pending_values().collect();
            for (slot, pending) in pending {
                let (bytes, owner) = self
                    .pending
                    .remove(&pending.id())
                    .expect("the pending value is known");
                assert_eq!(
                    owner,
                    Some(id),
                    "the value is appended for the page that holds it"
                );
                self.next += 1;
                let file = FileId::new(7).expect("a file");
                let pointer = ValuePointer::new(file, self.next * 4096, pending.value_len())
                    .expect("a value pointer");
                self.logged.insert(
                    pointer,
                    Logged {
                        owner: id,
                        bytes,
                        live: true,
                    },
                );
                mem.write(id, |page| {
                    let mut node = page.node_mut(slot).expect("the cell is there");
                    node.set_value(CellValue::OutOfLine(pointer))
                })
                .expect("the pointer takes the room of the pending value");
            }
        }
    }
}

impl Values for Log {
    fn released(&mut self, place: ValuePlace, page: PageId) {
        self.drop_place(place, page);
        self.released.push((place, page));
    }

    fn placed(&mut self, pending: PendingValue, page: PageId) {
        let entry = self
            .pending
            .get_mut(&pending.id())
            .expect("the write made the pending value");
        assert_eq!(entry.1.replace(page), None, "a pending value has one owner");
    }

    fn carried(&mut self, place: ValuePlace, from: PageId, to: PageId) -> PendingValue {
        let bytes = self.drop_place(place, from);
        self.carried.push((place, from, to));
        self.appended_again += 1;
        let pending = self.pending(&bytes);
        self.placed(pending, to);
        pending
    }
}
