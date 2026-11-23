//! Every page of the version, read once: its form, its own id, its
//! fragments, and the values its cells and records hold out of line.

use std::collections::BTreeSet;

use lignin_format::{
    flat_page::{FlatPage, FlatPageKind, FlatValue},
    tree_page::TreePage,
    value_log::{ValuePlace, ValuePointer},
    CellAddress, FormatVersion, PageId, SlotId,
};
use lignin_store::{
    alloc::PageState,
    cache::PageRef,
    load::{LoadError, FRAME_FORMAT},
};

use super::{read::ValueFault, Place, Rule, Run};

/// One bit for each slot of a page.
#[derive(Debug, Clone, Default)]
pub(super) struct Bits(Vec<u64>);

impl Bits {
    fn new(slots: usize) -> Self {
        Self(vec![0; slots.div_ceil(64)])
    }

    pub(super) fn get(&self, slot: SlotId) -> bool {
        let index = slot.index();
        (self.0.get(index / 64)).is_some_and(|word| word & (1 << (index % 64)) != 0)
    }

    /// Sets the bit of `slot`, and returns whether it was clear.
    fn set(&mut self, slot: SlotId) -> bool {
        let index = slot.index();
        let Some(word) = self.0.get_mut(index / 64) else {
            return false;
        };
        let mask = 1 << (index % 64);
        let was_clear = *word & mask == 0;
        *word |= mask;
        was_clear
    }
}

/// How a walk reached a cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Reach {
    /// From the root of a Merk in use.
    Live,
    /// From the record of a dropped Merk.
    Left,
}

/// A tree page of the version.
#[derive(Debug, Clone)]
pub(super) struct TreeSeen {
    /// The cells the page holds.
    pub(super) cells: u32,
    /// The cells a walk reached.
    pub(super) reached: u32,
    live: Bits,
    left: Bits,
}

impl TreeSeen {
    /// Whether a walk reached the cell in `slot`.
    pub(super) fn is_reached(&self, slot: SlotId) -> bool {
        self.live.get(slot) || self.left.get(slot)
    }

    /// Whether the cell in `slot` is of a Merk in use.
    pub(super) fn is_live(&self, slot: SlotId) -> bool {
        self.live.get(slot)
    }
}

/// How a walk reached a flat page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FlatReach {
    /// From the root of a keyspace.
    Tree,
    /// From the record of a lazy range delete.
    Left,
}

/// What the run knows of one page of the version.
#[derive(Debug, Clone)]
pub(super) enum Seen {
    /// A tree page.
    Tree(TreeSeen),
    /// A flat page, and how a walk reached it.
    Flat(Option<FlatReach>),
    /// A page that is not read or is not well formed. It was reported, and
    /// no walk enters it.
    Bad,
}

impl Seen {
    fn kind(&self) -> Kind {
        match self {
            Seen::Tree(_) => Kind::Tree,
            Seen::Flat(_) => Kind::Flat,
            Seen::Bad => Kind::Bad,
        }
    }
}

/// What a page is to a walk that an address led to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Kind {
    /// A tree page.
    Tree,
    /// A flat page.
    Flat,
    /// A page that was reported and is not entered.
    Bad,
    /// No page of the version has the id.
    None,
}

impl Run<'_, '_, '_> {
    /// Reads every page of the version: those the allocator holds an id
    /// for, and those the page table has a link for.
    pub(super) fn read_every_page(&mut self) {
        let mut candidates = BTreeSet::new();
        if let Some(ids) = self.store.ids {
            let (mut from, mut states) = (Some(PageId::MIN), Vec::new());
            while let Some(page) = from {
                states.clear();
                from = ids.scan(page, &mut states);
                candidates.extend(
                    (states.iter())
                        .filter(|(_, state)| *state != PageState::Private)
                        .map(|(page, _)| *page),
                );
            }
        }
        let (mut from, mut links) = (Some(PageId::MIN), Vec::new());
        while let Some(page) = from {
            links.clear();
            from = self.store.table.scan(page, &mut links);
            candidates.extend(links.iter().map(|(page, _)| *page));
        }
        for page in candidates {
            self.see(page);
        }
    }

    /// What the page `page` is, reading it if no part of the run has.
    pub(super) fn see(&mut self, page: PageId) -> Kind {
        if let Some(seen) = self.pages.get(&page) {
            return seen.kind();
        }
        self.begin_step();
        let frame = match self.frame(page) {
            Ok(frame) => frame,
            Err(LoadError::NoPage { .. }) => return Kind::None,
            Err(error) => {
                self.add(Rule::PageUnreadable, Place::Page(page), || {
                    error.to_string()
                });
                self.pages.insert(page, Seen::Bad);
                return Kind::Bad;
            }
        };
        let seen = match frame.first() {
            Some(&code) if code == TreePage::KIND_CODE => self.see_tree(page, &frame),
            Some(&code) if FlatPageKind::from_code(code, FRAME_FORMAT).is_ok() => {
                self.see_flat(page, &frame)
            }
            code => {
                let what = move || format!("a page of the unknown kind {code:?}");
                self.add(Rule::PageUnreadable, Place::Page(page), what);
                Seen::Bad
            }
        };
        let kind = seen.kind();
        self.pages.insert(page, seen);
        kind
    }

    fn see_tree(&mut self, id: PageId, frame: &PageRef<'_>) -> Seen {
        let place = Place::Page(id);
        let page = match TreePage::from_validated(frame, FRAME_FORMAT) {
            Ok(page) => page,
            Err(error) => {
                self.add(Rule::TreePageFormat, place, || error.to_string());
                return Seen::Bad;
            }
        };
        if let Err(error) = page.check(self.store.page_size) {
            self.add(Rule::TreePageFormat, place, || error.to_string());
            return Seen::Bad;
        }
        if page.id() != id {
            let found = page.id();
            self.add(Rule::PageId, place, || {
                format!("the page holds the id {found}")
            });
            return Seen::Bad;
        }
        if let Err(error) = page.check_fragments() {
            self.add(Rule::Fragments, place, || error.to_string());
        }
        for (slot, cell) in page.cells() {
            let Some(value) = cell.value_place() else {
                continue;
            };
            let holder = Place::Cell(CellAddress::new(id, slot));
            self.count_value(value, holder, frame);
        }
        self.census.tree_pages += 1;
        self.census.cells += page.cell_count() as u64;
        if page.is_empty() {
            self.unreached.empty_pages += 1;
        }
        Seen::Tree(TreeSeen {
            cells: page.cell_count() as u32,
            reached: 0,
            live: Bits::new(page.slot_count()),
            left: Bits::new(page.slot_count()),
        })
    }

    fn see_flat(&mut self, id: PageId, frame: &PageRef<'_>) -> Seen {
        let place = Place::Page(id);
        let page = match FlatPage::decode(frame, FRAME_FORMAT) {
            Ok(page) => page,
            Err(error) => {
                self.add(Rule::FlatPageFormat, place, || error.to_string());
                return Seen::Bad;
            }
        };
        if page.id() != id {
            let found = page.id();
            self.add(Rule::PageId, place, || {
                format!("the page holds the id {found}")
            });
            return Seen::Bad;
        }
        if let Some(leaf) = page.leaf() {
            for (slot, (_, value)) in leaf.iter().enumerate() {
                // The value of every record is read here, whatever reaches
                // its leaf: a keyspace, the record of a range delete, or
                // nothing.
                if let Some(value) = out_of_line(value) {
                    let holder = Place::FlatEntry { page: id, slot };
                    self.count_value(value, holder, frame);
                    self.check_value(value, id, holder);
                }
            }
        }
        self.census.flat_pages += 1;
        Seen::Flat(None)
    }

    /// Counts the value that `holder`, a cell or a record of the page in
    /// `frame`, holds out of line.
    fn count_value(&mut self, value: ValuePlace, holder: Place, frame: &PageRef<'_>) {
        match value {
            ValuePlace::Placed(pointer) => self.hold(pointer, holder),
            ValuePlace::Pending(pending) => {
                self.census.pending_values += 1;
                let found = frame.attached(pending.id()).map(<[u8]>::len);
                if found != Some(pending.value_len() as usize) {
                    self.add(Rule::PendingBytes, holder, || match found {
                        None => format!(
                            "a pending value of {} bytes has no bytes with its page",
                            pending.value_len()
                        ),
                        Some(found) => format!(
                            "a pending value of {} bytes has {found} bytes with its page",
                            pending.value_len()
                        ),
                    });
                }
            }
        }
    }

    /// Reads the value at `value`, which `holder`, a cell or a record of
    /// the page `page`, holds out of line, and reports what keeps it from
    /// being read.
    pub(super) fn check_value(&mut self, value: ValuePlace, page: PageId, holder: Place) {
        if let Err(fault) = self.values.fetch(value, page) {
            self.unread.insert(holder);
            if let Some(rule) = fault.rule() {
                self.add(rule, holder, || fault.to_string());
            }
        }
    }

    /// Reports that a later read of the value that `holder` holds out of
    /// line failed with `fault`, unless a read before it failed too and
    /// was reported ([`Self::check_value`]).
    pub(super) fn value_fault(&mut self, fault: &ValueFault, holder: Place) {
        if self.unread.insert(holder)
            && let Some(rule) = fault.rule()
        {
            self.add(rule, holder, || fault.to_string());
        }
    }

    fn hold(&mut self, pointer: ValuePointer, holder: Place) {
        self.census.placed_values += 1;
        let file = self.files.entry(pointer.file()).or_default();
        file.0 += 1;
        file.1 += pointer.extent_len(FormatVersion::CURRENT);
        if let Some(other) = self.holders.insert(pointer, holder) {
            self.add(Rule::ValueShared, holder, || {
                format!(
                    "the pointer to {} bytes at {} of value-log file {} is also held by {other}",
                    pointer.value_len(),
                    pointer.offset(),
                    pointer.file()
                )
            });
        }
    }

    /// Marks the cell at `address` as reached, and returns whether no walk
    /// had reached it. The page is a tree page that was seen.
    pub(super) fn reach(&mut self, address: CellAddress, how: Reach) -> bool {
        let Some(Seen::Tree(page)) = self.pages.get_mut(&address.page()) else {
            return false;
        };
        if page.is_reached(address.slot()) {
            return false;
        }
        let bits = match how {
            Reach::Live => &mut page.live,
            Reach::Left => &mut page.left,
        };
        if !bits.set(address.slot()) {
            return false;
        }
        page.reached += 1;
        match how {
            Reach::Live => self.census.live_cells += 1,
            Reach::Left => self.census.left_cells += 1,
        }
        true
    }
}

/// The place of the value that a record of a flat leaf holds out of line,
/// if it holds one.
pub(super) fn out_of_line(value: FlatValue<'_>) -> Option<ValuePlace> {
    let FlatValue::OutOfLine(bytes) = value else {
        return None;
    };
    // The page was decoded, so the 16 bytes are a place.
    (bytes.try_into().ok()).and_then(|bytes| ValuePlace::decode(bytes).ok())
}
