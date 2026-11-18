//! A model of a tree page that owns its parts, with its own layout of the
//! bytes, written from the specification and not from the codec.

use super::*;

pub(super) struct Rng(SplitMix64);

impl Rng {
    pub(super) fn new(seed: u64) -> Self {
        Self(SplitMix64::new(seed))
    }

    pub(super) fn below(&mut self, bound: usize) -> usize {
        (self.0.next_u64() % bound as u64) as usize
    }

    pub(super) fn chance(&mut self, percent: usize) -> bool {
        self.below(100) < percent
    }

    pub(super) fn bytes(&mut self, len: usize) -> Vec<u8> {
        self.0.bytes(len)
    }

    pub(super) fn hash(&mut self) -> Hash {
        let mut hash = [0; HASH_LEN];
        hash.copy_from_slice(&self.0.bytes(HASH_LEN));
        hash
    }

    /// An address in `own` page about half of the time.
    pub(super) fn address(&mut self, own: PageId) -> CellAddress {
        let page = match self.chance(50) {
            true => own,
            false => page_id(self.0.next_u64() as u32 | 1),
        };
        CellAddress::new(page, slot(1 + self.below(400) as u16))
    }

    pub(super) fn pointer(&mut self) -> ValuePointer {
        let offset = 20 + self.0.next_u64() % (1 << 40);
        pointer(
            1 + self.below(50) as u32,
            offset,
            1 + self.0.next_u64() as u32 % 100_000,
        )
    }

    fn short_len(&mut self, usual: usize, max: usize) -> usize {
        match self.below(20) {
            0 => 0,
            1 => max,
            _ => self.below(usual + 1),
        }
    }

    pub(super) fn link(&mut self, own: PageId) -> ModelLink {
        let aggregate_len = match self.below(12) {
            0 => 255,
            1..=4 => [9, 17, 25, 1][self.below(4)],
            _ => 0,
        };
        ModelLink {
            child: self.address(own),
            hash: self.hash(),
            heights: (self.below(256) as u8, self.below(256) as u8),
            child_key_len: self.below(256) as u8,
            aggregate: self.bytes(aggregate_len),
        }
    }

    pub(super) fn value(&mut self, max_inline: usize) -> ModelValue {
        match self.below(10) {
            0..=3 => ModelValue::OutOfLine(self.pointer()),
            4 => ModelValue::Inline(Vec::new()),
            5 => {
                let len = self.below(max_inline + 1);
                ModelValue::Inline(self.bytes(len))
            }
            _ => {
                let len = self.below(max_inline.min(300) + 1);
                ModelValue::Inline(self.bytes(len))
            }
        }
    }

    pub(super) fn hosted(&mut self, own: PageId) -> Vec<Option<CellAddress>> {
        let count = match self.below(40) {
            0 => 255,
            1..=8 => 1 + self.below(4),
            _ => 0,
        };
        (0..count)
            .map(|_| self.chance(70).then(|| self.address(own)))
            .collect()
    }

    /// A cell with inline values of up to `max_inline` bytes.
    pub(super) fn cell(&mut self, own: PageId, max_inline: usize) -> ModelCell {
        let key_len = self.short_len(40, 255);
        let feature_len = self.short_len(27, 255);
        ModelCell {
            key: self.bytes(key_len),
            feature: self.bytes(feature_len),
            value_hash: self.hash(),
            kv_hash: self.hash(),
            left: self.chance(50).then(|| self.link(own)),
            right: self.chance(50).then(|| self.link(own)),
            hosted: self.hosted(own),
            value: self.value(max_inline),
            fragment_root: self.chance(30),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ModelLink {
    pub(super) child: CellAddress,
    pub(super) hash: Hash,
    pub(super) heights: (u8, u8),
    pub(super) child_key_len: u8,
    pub(super) aggregate: Vec<u8>,
}

impl ModelLink {
    pub(super) fn link(&self) -> TreeLink<'_> {
        TreeLink {
            child: self.child,
            hash: &self.hash,
            heights: self.heights,
            child_key_len: self.child_key_len,
            aggregate: &self.aggregate,
        }
    }

    fn read(link: TreeLink<'_>) -> Self {
        Self {
            child: link.child,
            hash: *link.hash,
            heights: link.heights,
            child_key_len: link.child_key_len,
            aggregate: link.aggregate.to_vec(),
        }
    }

    fn assemble(&self, out: &mut Vec<u8>) {
        out.extend(self.child.page().get().to_le_bytes());
        out.extend(self.child.slot().get().to_le_bytes());
        out.extend(self.hash);
        out.extend([self.heights.0, self.heights.1, self.child_key_len]);
        out.push(self.aggregate.len() as u8);
        out.extend(&self.aggregate);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ModelValue {
    Inline(Vec<u8>),
    OutOfLine(ValuePointer),
}

impl ModelValue {
    pub(super) fn value(&self) -> CellValue<'_> {
        match self {
            Self::Inline(value) => CellValue::Inline(value),
            Self::OutOfLine(pointer) => CellValue::OutOfLine(*pointer),
        }
    }

    pub(super) fn read(value: CellValue<'_>) -> Self {
        match value {
            CellValue::Inline(value) => Self::Inline(value.to_vec()),
            CellValue::OutOfLine(pointer) => Self::OutOfLine(pointer),
        }
    }

    pub(super) fn value_len(&self) -> u32 {
        match self {
            Self::Inline(value) => value.len() as u32,
            Self::OutOfLine(pointer) => pointer.value_len(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ModelCell {
    pub(super) key: Vec<u8>,
    pub(super) feature: Vec<u8>,
    pub(super) value_hash: Hash,
    pub(super) kv_hash: Hash,
    pub(super) left: Option<ModelLink>,
    pub(super) right: Option<ModelLink>,
    pub(super) hosted: Vec<Option<CellAddress>>,
    pub(super) value: ModelValue,
    pub(super) fragment_root: bool,
}

impl ModelCell {
    /// A leaf with nothing but a key and a value.
    pub(super) fn leaf(key: &[u8], value: &[u8]) -> Self {
        Self {
            key: key.to_vec(),
            feature: Vec::new(),
            value_hash: [0xA1; 32],
            kv_hash: [0xB2; 32],
            left: None,
            right: None,
            hosted: Vec::new(),
            value: ModelValue::Inline(value.to_vec()),
            fragment_root: false,
        }
    }

    pub(super) fn content(&self) -> CellContent<'_> {
        CellContent {
            key: &self.key,
            feature: &self.feature,
            value_hash: &self.value_hash,
            kv_hash: &self.kv_hash,
            left: self.left.as_ref().map(ModelLink::link),
            right: self.right.as_ref().map(ModelLink::link),
            hosted_roots: &self.hosted,
            value: self.value.value(),
            fragment_root: self.fragment_root,
        }
    }

    pub(super) fn link(&self, side: Side) -> &Option<ModelLink> {
        match side {
            Side::Left => &self.left,
            Side::Right => &self.right,
        }
    }

    pub(super) fn link_mut(&mut self, side: Side) -> &mut Option<ModelLink> {
        match side {
            Side::Left => &mut self.left,
            Side::Right => &mut self.right,
        }
    }

    pub(super) fn hash_mut(&mut self, field: HashField) -> Option<&mut Hash> {
        match field {
            HashField::Value => Some(&mut self.value_hash),
            HashField::KeyValue => Some(&mut self.kv_hash),
            HashField::Child(side) => self.link_mut(side).as_mut().map(|link| &mut link.hash),
        }
    }

    pub(super) fn hash(&self, field: HashField) -> Option<&Hash> {
        match field {
            HashField::Value => Some(&self.value_hash),
            HashField::KeyValue => Some(&self.kv_hash),
            HashField::Child(side) => self.link(side).as_ref().map(|link| &link.hash),
        }
    }

    /// What every accessor of `cell` returns.
    pub(super) fn read(cell: TreeCell<'_>) -> Self {
        Self {
            key: cell.key().to_vec(),
            feature: cell.feature().to_vec(),
            value_hash: *cell.value_hash(),
            kv_hash: *cell.kv_hash(),
            left: cell.link(Side::Left).map(ModelLink::read),
            right: cell.link(Side::Right).map(ModelLink::read),
            hosted: cell.hosted_roots().collect(),
            value: ModelValue::read(cell.value()),
            fragment_root: cell.is_fragment_root(),
        }
    }

    /// The cell's bytes, laid out by hand from the specification.
    pub(super) fn assemble(&self) -> Vec<u8> {
        let mut flags = 0;
        if self.left.is_some() {
            flags |= 1;
        }
        if self.right.is_some() {
            flags |= 2;
        }
        if matches!(self.value, ModelValue::OutOfLine(_)) {
            flags |= 4;
        }
        if self.fragment_root {
            flags |= 8;
        }
        let mut out = vec![
            flags,
            self.key.len() as u8,
            self.feature.len() as u8,
            self.hosted.len() as u8,
        ];
        out.extend(self.value.value_len().to_le_bytes());
        out.extend(self.value_hash);
        out.extend(self.kv_hash);
        for link in [&self.left, &self.right].into_iter().flatten() {
            link.assemble(&mut out);
        }
        for root in &self.hosted {
            match root {
                Some(root) => {
                    out.extend(root.page().get().to_le_bytes());
                    out.extend(root.slot().get().to_le_bytes());
                }
                None => out.extend([0; 6]),
            }
        }
        out.extend(&self.feature);
        out.extend(&self.key);
        match &self.value {
            ModelValue::Inline(value) => out.extend(value),
            ModelValue::OutOfLine(pointer) => {
                out.extend(pointer.file().get().to_le_bytes());
                out.extend(pointer.offset().to_le_bytes());
            }
        }
        out
    }

    pub(super) fn len(&self) -> usize {
        self.assemble().len()
    }

    /// Checks every accessor of `cell` against this cell.
    pub(super) fn assert_is(&self, cell: TreeCell<'_>) {
        assert_eq!(Self::read(cell), *self);
        let bytes = self.assemble();
        assert_eq!(cell.as_bytes(), bytes);
        assert_eq!(cell.encoded_len(), bytes.len());
        assert_eq!(self.content().encoded_len(), bytes.len());
        assert_eq!(cell.value_len(), self.value.value_len());
        assert_eq!(cell.value_pointer(), self.value.value().pointer());
        let value = self.value.value();
        assert_eq!(cell.value().inline(), value.inline());
        assert_eq!(cell.value().is_inline(), value.pointer().is_none());
        assert_eq!(cell.value().stored_len(), value.stored_len());
        let count = self.hosted.len();
        assert_eq!(cell.hosted_root_count(), count);
        assert_eq!(cell.hosted_roots().len(), count);
        assert_eq!(cell.hosted_roots().size_hint(), (count, Some(count)));
        let mut backwards: Vec<_> = cell.hosted_roots().rev().collect();
        backwards.reverse();
        assert_eq!(backwards, self.hosted);
        assert_eq!(cell.hosted_roots().nth(count), None);
        if let Some(last) = count.checked_sub(1) {
            assert_eq!(cell.hosted_roots().nth(last), Some(self.hosted[last]));
        }
        for field in HashField::ALL {
            assert_eq!(cell.hash(field), self.hash(field));
        }
        for side in Side::BOTH {
            assert_eq!(cell.has_link(side), self.link(side).is_some());
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ModelPage {
    pub(super) id: PageId,
    pub(super) departures: u64,
    pub(super) cells: BTreeMap<u16, ModelCell>,
}

impl ModelPage {
    pub(super) fn new(id: u32, departures: u64) -> Self {
        Self {
            id: page_id(id),
            departures,
            cells: BTreeMap::new(),
        }
    }

    pub(super) fn slot_count(&self) -> usize {
        self.cells.keys().next_back().map_or(0, |&raw| raw.into())
    }

    pub(super) fn lowest_free_slot(&self) -> u16 {
        (1..)
            .find(|raw| !self.cells.contains_key(raw))
            .expect("a free slot")
    }

    /// The length of the page's image.
    pub(super) fn image_len(&self) -> usize {
        22 + 2 * self.slot_count() + self.cells.values().map(ModelCell::len).sum::<usize>()
    }

    pub(super) fn free_bytes(&self, size: TreePageSize) -> usize {
        size.image_capacity() - self.image_len()
    }

    /// The page's image, laid out by hand from the specification.
    pub(super) fn assemble(&self) -> Vec<u8> {
        let slot_count = self.slot_count();
        let mut directory = vec![0u8; 2 * slot_count];
        let mut cells = Vec::new();
        for (&raw, cell) in &self.cells {
            let offset = (22 + directory.len() + cells.len()) as u16;
            let at = 2 * (usize::from(raw) - 1);
            directory[at..at + 2].copy_from_slice(&offset.to_le_bytes());
            cells.extend(cell.assemble());
        }
        let len = (22 + directory.len() + cells.len()) as u16;
        let mut out = vec![3, 0];
        out.extend(len.to_le_bytes());
        out.extend(self.id.get().to_le_bytes());
        out.extend((slot_count as u16).to_le_bytes());
        out.extend((self.cells.len() as u16).to_le_bytes());
        out.extend([0, 0]);
        out.extend(self.departures.to_le_bytes());
        out.extend(directory);
        out.extend(cells);
        out
    }

    /// The slots of the cells no cell of the page points at.
    pub(super) fn unnamed(&self) -> Vec<u16> {
        let named: Vec<CellAddress> = self
            .cells
            .values()
            .flat_map(|cell| {
                let children = [&cell.left, &cell.right]
                    .into_iter()
                    .flatten()
                    .map(|link| link.child);
                children.chain(cell.hosted.iter().flatten().copied())
            })
            .collect();
        self.cells
            .keys()
            .copied()
            .filter(|&raw| !named.contains(&CellAddress::new(self.id, slot(raw))))
            .collect()
    }

    /// Checks every accessor of `page` against this page, and that its image
    /// is this page's and reads back.
    pub(super) fn assert_is(&self, page: TreePage<'_>, size: TreePageSize) {
        page.check(size).expect("a valid page");
        assert_eq!(page.version(), V1);
        assert_eq!(page.id(), self.id);
        assert_eq!(page.departures(), self.departures);
        assert_eq!(page.cell_count(), self.cells.len());
        assert_eq!(page.is_empty(), self.cells.is_empty());
        assert_eq!(page.slot_count(), self.slot_count());
        assert_eq!(page.used_bytes(), self.image_len());
        assert_eq!(page.free_bytes(size), self.free_bytes(size));
        assert_eq!(page.as_bytes().len(), page.encoded_len());
        assert_eq!(page.encoded_len() - page.unused_bytes(), self.image_len());
        assert_eq!(page.lowest_free_slot().get(), self.lowest_free_slot());

        for raw in 1..=self.slot_count() as u16 + 2 {
            match self.cells.get(&raw) {
                Some(cell) => cell.assert_is(page.cell(slot(raw)).expect("a cell")),
                None => assert!(page.cell(slot(raw)).is_none()),
            }
        }
        assert!(page.cell(SlotId::MAX).is_none());
        let slots: Vec<u16> = self.cells.keys().copied().collect();
        let listed: Vec<u16> = page.cells().map(|(slot, _)| slot.get()).collect();
        assert_eq!(listed, slots);
        let mut backwards: Vec<u16> = page.cells().rev().map(|(slot, _)| slot.get()).collect();
        backwards.reverse();
        assert_eq!(backwards, slots);
        for (slot, cell) in page.cells() {
            self.cells[&slot.get()].assert_is(cell);
            let address = CellAddress::new(self.id, slot);
            assert_eq!(
                page.cell_by_address(address).map(|cell| cell.as_bytes()),
                Some(cell.as_bytes())
            );
        }
        let roots: Vec<u16> = page.fragment_roots().map(SlotId::get).collect();
        let marked: Vec<u16> = self
            .cells
            .iter()
            .filter(|(_, cell)| cell.fragment_root)
            .map(|(&raw, _)| raw)
            .collect();
        assert_eq!(roots, marked);

        let image = page.to_image();
        assert_eq!(image, self.assemble());
        let decoded = TreePage::decode(&image, V1, size).expect("the image decodes");
        assert_eq!(decoded.as_bytes(), image);
        assert_eq!(decoded.unused_bytes(), 0);
        assert_eq!(decoded.to_image(), image);
        let mut appended = vec![0xAB; 3];
        decoded.write_image(&mut appended);
        assert_eq!(appended[..3], [0xAB; 3]);
        assert_eq!(appended[3..], image);
        for (&raw, cell) in &self.cells {
            cell.assert_is(decoded.cell(slot(raw)).expect("a cell"));
        }
    }
}
