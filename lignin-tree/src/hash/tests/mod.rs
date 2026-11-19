mod acceptance;
mod errors;
mod evaluation;
mod grove;
mod marks;
mod property;
mod store;

use std::collections::{BTreeMap, HashMap};

use lignin_format::{
    delta::Delta,
    tree_page::{CellValue, TreePageMut},
    value_log::{PendingValue, ValuePlace, ValuePointer},
    FileId, PageId, PageSize, SlotId,
};
use lignin_store::load::FRAME_FORMAT;

use super::*;
use crate::{
    fragment::{FragmentError, PageMut, Values},
    node::{ChangeSink, Lease, NodeError, NodePage, NodePageMut, NodeRef},
};

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

/// A hash of the tests: every byte of every part, and where each part ends.
fn digest(parts: &[&[u8]]) -> Hash {
    let mut lanes = [
        0x243F_6A88_85A3_08D3u64,
        0x1319_8A2E_0370_7344,
        0xA409_3822_299F_31D0,
        0x082E_FA98_EC4E_6C89,
    ];
    for part in parts {
        for (index, byte) in part.iter().enumerate() {
            let lane = &mut lanes[index % 4];
            *lane = (*lane ^ u64::from(*byte))
                .wrapping_mul(0x0000_0100_0000_01B3)
                .rotate_left(23);
        }
        lanes.rotate_left(1);
        lanes[0] ^= part.len() as u64;
        for lane in &mut lanes {
            *lane = lane.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ (*lane >> 29);
        }
    }
    let mut hash = [0; 32];
    for (chunk, lane) in hash.chunks_mut(8).zip(lanes) {
        chunk.copy_from_slice(&lane.to_le_bytes());
    }
    hash
}

/// The value hash of the tests: the value, and the root of every hosted
/// Merk in its place.
fn value_rule(value: &[u8], hosted: &[Option<Hash>]) -> Hash {
    let mut parts: Vec<&[u8]> = vec![b"value", value];
    for root in hosted {
        parts.push(match root {
            Some(hash) => hash,
            None => b"no root",
        });
    }
    digest(&parts)
}

fn kv_rule(key: &[u8], value_hash: &Hash) -> Hash {
    digest(&[b"kv", key, value_hash])
}

/// The hash of a node of the tests. It takes in the feature and what each
/// link says of its child beside the hash, so a cell that is not up to date
/// gives another hash.
fn node_rule(
    kv_hash: &Hash,
    feature: &[u8],
    left: Option<(&Hash, &[u8])>,
    right: Option<(&Hash, &[u8])>,
) -> Hash {
    let (no_hash, no_aggregate): (&Hash, &[u8]) = (&[0; 32], b"no child");
    let (left_hash, left_aggregate) = left.unwrap_or((no_hash, no_aggregate));
    let (right_hash, right_aggregate) = right.unwrap_or((no_hash, no_aggregate));
    digest(&[
        b"node",
        kv_hash,
        feature,
        left_hash,
        left_aggregate,
        right_hash,
        right_aggregate,
    ])
}

fn node_rule_of(node: NodeRef<'_>) -> Hash {
    let link = |side| node.link(side).map(|link| (link.hash, link.aggregate));
    node_rule(
        node.kv_hash(),
        node.feature(),
        link(Side::Left),
        link(Side::Right),
    )
}

/// Which rule was called.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum Rule {
    Value,
    Kv,
    Node,
}

/// The rules of the tests, which keep every call: the rule, and the key of
/// the node it was called for.
#[derive(Debug, Default)]
struct Counting {
    calls: Vec<(Rule, Vec<u8>)>,
    /// The call that is refused.
    refuse: Option<(Rule, Vec<u8>)>,
}

impl Counting {
    fn called(&mut self, rule: Rule, key: &[u8]) -> Result<(), Refusal> {
        if self.refuse.as_ref() == Some(&(rule, key.to_vec())) {
            return Err(format!("no {rule:?} hash for this node").into());
        }
        self.calls.push((rule, key.to_vec()));
        Ok(())
    }

    /// The calls since `from`, sorted.
    fn since(&self, from: usize) -> Vec<(Rule, Vec<u8>)> {
        let mut calls = self.calls[from..].to_vec();
        calls.sort();
        calls
    }
}

impl HashRules for Counting {
    fn value_hash(
        &mut self,
        node: NodeRef<'_>,
        value: &[u8],
        hosted: &[Option<Hash>],
    ) -> Result<Hash, Refusal> {
        self.called(Rule::Value, node.key())?;
        assert_eq!(value.len(), node.value_len() as usize, "the whole value");
        assert_eq!(hosted.len(), node.hosted_roots().len());
        let roots = node.hosted_roots().zip(hosted);
        assert!(roots
            .into_iter()
            .all(|(root, hash)| root.is_some() == hash.is_some()));
        Ok(value_rule(value, hosted))
    }

    fn kv_hash(&mut self, key: &[u8], value_hash: &Hash) -> Result<Hash, Refusal> {
        self.called(Rule::Kv, key)?;
        Ok(kv_rule(key, value_hash))
    }

    fn node_hash(&mut self, node: NodeRef<'_>) -> Result<Hash, Refusal> {
        self.called(Rule::Node, node.key())?;
        Ok(node_rule_of(node))
    }
}

/// The same rules, with nothing kept.
#[derive(Debug, Default)]
struct Plain;

impl HashRules for Plain {
    fn value_hash(
        &mut self,
        _: NodeRef<'_>,
        value: &[u8],
        hosted: &[Option<Hash>],
    ) -> Result<Hash, Refusal> {
        Ok(value_rule(value, hosted))
    }

    fn kv_hash(&mut self, key: &[u8], value_hash: &Hash) -> Result<Hash, Refusal> {
        Ok(kv_rule(key, value_hash))
    }

    fn node_hash(&mut self, node: NodeRef<'_>) -> Result<Hash, Refusal> {
        Ok(node_rule_of(node))
    }
}

/// One step on one page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    Read(PageId),
    Write(PageId),
}

/// A hash record, as the sink was told it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct HashRecord {
    at: CellAddress,
    field: HashField,
    hash: Hash,
}

/// The sink of the tests. It makes every record it is told on a copy of the
/// page, so the copies are what the records alone give, and keeps the hash
/// records.
#[derive(Debug)]
struct Shadow {
    size: PageSize,
    frames: BTreeMap<PageId, Vec<u8>>,
    hashes: Vec<HashRecord>,
    /// How many records were told that are not hash records.
    others: usize,
}

impl ChangeSink for Shadow {
    fn created(&mut self, page: PageId, departures: u64) {
        let mut frame = vec![0; self.size.get()];
        TreePageMut::init(&mut frame, self.size, page, departures).expect("a frame of a page");
        assert!(self.frames.insert(page, frame).is_none());
        self.others += 1;
    }

    fn changed(&mut self, page: PageId, change: &Delta<'_>) {
        let frame = self.frames.get_mut(&page).expect("the page was started");
        let mut copy = TreePageMut::open(frame, self.size, FRAME_FORMAT).expect("a tree page");
        change.apply(&mut copy).expect("a record its page took");
        match *change {
            Delta::Hash { slot, field, hash } => {
                assert_eq!(change.encoded_len(), 35, "a hash record");
                self.hashes.push(HashRecord {
                    at: CellAddress::new(page, slot),
                    field,
                    hash: *hash,
                });
            }
            _ => self.others += 1,
        }
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
    /// The steps that were made, in order.
    steps: Vec<Step>,
    /// How many more steps the pages take before they fail.
    steps_left: Option<usize>,
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
                hashes: Vec::new(),
                others: 0,
            },
            next: 1,
            steps: Vec::new(),
            steps_left: None,
            behind: Vec::new(),
        }
    }

    fn view(&self, page: PageId) -> NodePage<'_> {
        NodePage::open(&self.frames[&page], self.lease).expect("a tree page")
    }

    fn node(&self, at: CellAddress) -> NodeRef<'_> {
        let node = self.view(at.page()).node(at.slot());
        node.expect("a stored address names a cell")
    }

    fn step(&mut self, step: Step) -> Result<(), FragmentError> {
        if let Some(left) = &mut self.steps_left {
            *left = left
                .checked_sub(1)
                .ok_or_else(|| FragmentError::Pages("the pages are closed".into()))?;
        }
        self.steps.push(step);
        Ok(())
    }

    fn missing(page: PageId) -> FragmentError {
        FragmentError::Pages(format!("no {page}").into())
    }

    /// Every page is what its records alone give.
    fn check_records(&self) {
        assert_eq!(self.shadow.frames.len(), self.frames.len());
        for (id, frame) in &self.shadow.frames {
            let shadow = NodePage::open(frame, self.lease).expect("a page");
            assert_eq!(
                shadow.tree_page().to_image(),
                self.view(*id).tree_page().to_image(),
                "the records of {id} give the page"
            );
        }
    }
}

impl Pages for Mem {
    fn read<R>(
        &mut self,
        page: PageId,
        read: impl FnOnce(NodePage<'_>) -> R,
    ) -> Result<R, FragmentError> {
        self.step(Step::Read(page))?;
        let frame = self.frames.get(&page).ok_or_else(|| Self::missing(page))?;
        Ok(read(NodePage::open(frame, self.lease)?))
    }

    fn write<R>(
        &mut self,
        page: PageId,
        change: impl FnOnce(&mut PageMut<'_, '_>) -> Result<R, NodeError>,
    ) -> Result<R, FragmentError> {
        self.step(Step::Write(page))?;
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

    fn free(&mut self, page: PageId) -> Result<(), FragmentError> {
        self.frames
            .remove(&page)
            .ok_or_else(|| Self::missing(page))?;
        self.shadow.frames.remove(&page);
        Ok(())
    }

    fn host_is_behind(&mut self, host: CellAddress) -> bool {
        self.behind.contains(&host)
    }
}

/// The values of the tests that are not in their cells: those a commit has
/// not put into the value log yet, and those it has.
#[derive(Debug, Default)]
struct Log {
    pending: HashMap<[u8; 8], Vec<u8>>,
    placed: HashMap<ValuePointer, Vec<u8>>,
    next: u64,
    /// How many values were read.
    reads: usize,
    /// Gives every read one byte too many.
    long: bool,
}

impl Log {
    /// A value that the next write gives to a cell out of line.
    fn pending(&mut self, bytes: &[u8]) -> PendingValue {
        self.next += 1;
        let id = self.next.to_le_bytes();
        self.pending.insert(id, bytes.to_vec());
        PendingValue::new(id, bytes.len() as u32).expect("a pending value")
    }

    fn bytes(&self, place: ValuePlace) -> Option<&Vec<u8>> {
        match place {
            ValuePlace::Placed(pointer) => self.placed.get(&pointer),
            ValuePlace::Pending(pending) => self.pending.get(&pending.id()),
        }
    }

    /// The bytes of the value a cell holds.
    fn value(&self, value: CellValue<'_>) -> Vec<u8> {
        match value.place() {
            None => value.inline().expect("a value in its cell").to_vec(),
            Some(place) => self.bytes(place).expect("a value that is kept").clone(),
        }
    }

    /// Write-back: every pending value of the pages goes to the value log,
    /// and its pointer into its cell.
    fn write_back(&mut self, mem: &mut Mem) {
        let pages: Vec<PageId> = mem.frames.keys().copied().collect();
        for id in pages {
            let pending: Vec<_> = mem.view(id).tree_page().pending_values().collect();
            for (slot, pending) in pending {
                let bytes = self.pending.remove(&pending.id()).expect("a kept value");
                self.next += 1;
                let file = FileId::new(7).expect("a file");
                let pointer = ValuePointer::new(file, self.next * 4096, pending.value_len())
                    .expect("a value pointer");
                self.placed.insert(pointer, bytes);
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
    fn released(&mut self, place: ValuePlace, _: PageId) {
        match place {
            ValuePlace::Placed(pointer) => drop(self.placed.remove(&pointer)),
            ValuePlace::Pending(pending) => drop(self.pending.remove(&pending.id())),
        }
    }

    fn placed(&mut self, _: PendingValue, _: PageId) {}

    fn carried(&mut self, place: ValuePlace, from: PageId, _: PageId) -> PendingValue {
        let bytes = self.bytes(place).expect("a kept value").clone();
        self.released(place, from);
        self.pending(&bytes)
    }
}

impl ValueReader for Log {
    fn read(&mut self, place: ValuePlace, _: PageId, into: &mut Vec<u8>) -> Result<(), Refusal> {
        assert!(into.is_empty(), "the reader is given an empty buffer");
        let bytes = self.bytes(place).ok_or("the value is not kept")?;
        into.extend_from_slice(bytes);
        into.extend(self.long.then_some(0));
        self.reads += 1;
        Ok(())
    }
}
