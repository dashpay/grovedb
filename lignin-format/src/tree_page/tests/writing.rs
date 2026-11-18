//! Writing: every change through `TreePageMut`, against the model.

use super::*;

/// One page in a frame beside its model, changed together.
struct Harness {
    frame: Vec<u8>,
    model: ModelPage,
    size: PageSize,
    rng: Rng,
}

impl Harness {
    fn new(seed: u64, size: PageSize) -> Self {
        let model = ModelPage::new(17, 5);
        Self {
            frame: frame_of(&model, size),
            model,
            size,
            rng: Rng::new(seed),
        }
    }

    fn page(&mut self) -> TreePageMut<'_> {
        open(&mut self.frame, self.size)
    }

    fn assert_matches(&self) {
        self.model.assert_is(view(&self.frame), self.size);
    }

    fn occupied(&mut self) -> Option<u16> {
        let slots: Vec<u16> = self.model.cells.keys().copied().collect();
        (!slots.is_empty()).then(|| slots[self.rng.below(slots.len())])
    }

    /// What a write must return that takes a cell from `old_len` to
    /// `new_len` bytes and adds `new_slots` slots to the directory.
    fn expected(
        &self,
        old_len: usize,
        new_len: usize,
        new_slots: usize,
    ) -> Result<(), FormatError> {
        let max = self.size.max_cell_len();
        if new_len != old_len && new_len > max {
            return Err(TreePageError::CellTooLarge { len: new_len, max }.into());
        }
        let needed = new_len.saturating_sub(old_len) + 2 * new_slots;
        let available = self.model.free_bytes(self.size);
        if needed > available {
            return Err(TreePageError::PageFull { needed, available }.into());
        }
        Ok(())
    }

    /// Runs `write` on the page. It must return what the model expects for
    /// `changed` taking the place of the cell in `raw`, and on an error it
    /// must leave every byte of the page as it was.
    fn change(
        &mut self,
        raw: u16,
        changed: ModelCell,
        write: impl FnOnce(&mut TreePageMut<'_>, &ModelCell) -> Result<(), FormatError>,
    ) {
        let old_len = self.model.cells.get(&raw).map_or(0, ModelCell::len);
        let new_slots = usize::from(raw).saturating_sub(self.model.slot_count());
        let expected = self.expected(old_len, changed.len(), new_slots);
        let before = self.frame.clone();
        let result = write(&mut self.page(), &changed);
        assert_eq!(result, expected);
        match result {
            Ok(()) => drop(self.model.cells.insert(raw, changed)),
            Err(_) => assert!(self.frame == before, "a refused write changed the page"),
        }
    }

    fn insert(&mut self) {
        let raw = match self.rng.chance(75) {
            true => self.model.lowest_free_slot(),
            false => 1 + self.rng.below(self.model.slot_count() + 5) as u16,
        };
        let max_inline = match self.rng.chance(4) {
            true => self.size.max_cell_len() + 600,
            false => 300,
        };
        let cell = self.rng.cell(self.model.id, max_inline);
        if self.model.cells.contains_key(&raw) {
            let before = self.frame.clone();
            let result = self.page().insert(slot(raw), &cell.content());
            let max = self.size.max_cell_len();
            let expected = match cell.len() > max {
                true => TreePageError::CellTooLarge {
                    len: cell.len(),
                    max,
                },
                false => TreePageError::SlotOccupied { slot: slot(raw) },
            };
            assert_eq!(result, Err(expected.into()));
            assert!(self.frame == before);
        } else if self.rng.chance(50) {
            self.change(raw, cell, |page, cell| {
                page.insert(slot(raw), &cell.content())
            });
        } else {
            self.change(raw, cell, |page, cell| {
                let bytes = cell.content().encode()?;
                page.insert_encoded(slot(raw), TreeCell::decode(&bytes, V1)?)
            });
        }
    }

    fn remove(&mut self) {
        if self.rng.chance(10) {
            let free = match self.rng.chance(50) {
                true => self.model.lowest_free_slot(),
                false => self.model.slot_count() as u16 + 1 + self.rng.below(60_000) as u16,
            };
            let before = self.frame.clone();
            assert_eq!(
                self.page().remove(slot(free)),
                Err(TreePageError::SlotFree { slot: slot(free) }.into())
            );
            assert!(self.frame == before);
        } else if let Some(raw) = self.occupied() {
            self.page().remove(slot(raw)).expect("occupied");
            self.model.cells.remove(&raw);
            self.model.departures += 1;
        }
    }

    fn step(&mut self) {
        let Some(raw) = self.occupied() else {
            return self.insert();
        };
        let slot = slot(raw);
        let own = self.model.id;
        let mut cell = self.model.cells[&raw].clone();
        match self.rng.below(100) {
            0..30 => self.insert(),
            30..45 => self.remove(),
            45..52 => {
                let mut new = self.rng.cell(own, 300);
                if self.rng.chance(10) && new.key != cell.key {
                    // Another node does not take the slot this way.
                    let before = self.frame.clone();
                    let refused = Err(TreePageError::KeyChanged { slot }.into());
                    assert_eq!(self.page().replace(slot, &new.content()), refused);
                    let bytes = new.assemble();
                    let other = TreeCell::decode(&bytes, V1).expect("valid");
                    assert_eq!(self.page().replace_encoded(slot, other), refused);
                    assert!(self.frame == before);
                    return;
                }
                new.key = cell.key.clone();
                match self.rng.chance(50) {
                    true => self.change(raw, new, |page, new| page.replace(slot, &new.content())),
                    false => self.change(raw, new, |page, new| {
                        let bytes = new.content().encode()?;
                        page.replace_encoded(slot, TreeCell::decode(&bytes, V1)?)
                    }),
                }
            }
            52..62 => {
                let field = HashField::ALL[self.rng.below(4)];
                let hash = self.rng.hash();
                let result = self.page().set_hash(slot, field, &hash);
                match (cell.hash_mut(field), field) {
                    (Some(stored), _) => {
                        *stored = hash;
                        result.expect("a stored hash");
                        self.model.cells.insert(raw, cell);
                    }
                    (None, HashField::Child(side)) => {
                        assert_eq!(result, Err(TreePageError::NoSuchLink { slot, side }.into()))
                    }
                    (None, _) => unreachable!("a node always has its own hashes"),
                }
            }
            62..67 => {
                let side = Side::BOTH[self.rng.below(2)];
                let child = self.rng.address(own);
                let result = self.page().set_child(slot, side, child);
                match cell.link_mut(side) {
                    Some(link) => {
                        link.child = child;
                        result.expect("a link");
                        self.model.cells.insert(raw, cell);
                    }
                    None => {
                        assert_eq!(result, Err(TreePageError::NoSuchLink { slot, side }.into()))
                    }
                }
            }
            67..78 => {
                let side = Side::BOTH[self.rng.below(2)];
                let mut link = self.rng.chance(75).then(|| self.rng.link(own));
                // Often a link like the one it replaces, as a hash update is.
                if let (Some(new), Some(old)) = (&mut link, cell.link(side))
                    && self.rng.chance(60)
                {
                    new.aggregate = self.rng.bytes(old.aggregate.len());
                }
                *cell.link_mut(side) = link;
                self.change(raw, cell, |page, cell| {
                    page.set_link(
                        slot,
                        side,
                        cell.link(side).as_ref().map(ModelLink::link).as_ref(),
                    )
                });
            }
            78..82 => {
                cell.fragment_root = self.rng.chance(50);
                self.page()
                    .set_fragment_root(slot, cell.fragment_root)
                    .expect("occupied");
                self.model.cells.insert(raw, cell);
            }
            82..86 => {
                let index = self.rng.below(cell.hosted.len() + 2);
                let root = self.rng.chance(60).then(|| self.rng.address(own));
                let result = self.page().set_hosted_root(slot, index, root);
                match cell.hosted.get_mut(index) {
                    Some(stored) => {
                        *stored = root;
                        result.expect("a hosted root");
                        self.model.cells.insert(raw, cell);
                    }
                    None => assert_eq!(
                        result,
                        Err(TreePageError::NoSuchHostedRoot { slot, index }.into())
                    ),
                }
            }
            86..89 => {
                cell.hosted = self.rng.hosted(own);
                self.change(raw, cell, |page, cell| {
                    page.set_hosted_roots(slot, &cell.hosted)
                });
            }
            89..92 => {
                let len = match self.rng.chance(50) {
                    true => cell.feature.len(),
                    false => self.rng.below(30),
                };
                cell.feature = self.rng.bytes(len);
                self.change(raw, cell, |page, cell| {
                    page.set_feature(slot, &cell.feature)
                });
            }
            92..98 => {
                let max_inline = match self.rng.chance(4) {
                    true => self.size.max_cell_len() + 600,
                    false => 300,
                };
                cell.value = self.rng.value(max_inline);
                self.change(raw, cell, |page, cell| {
                    page.set_value(slot, cell.value.value())
                });
            }
            98 => {
                self.page().compact().expect("a valid page");
                let page = view(&self.frame);
                assert_eq!(page.unused_bytes(), 0);
                assert_eq!(page.as_bytes(), self.model.assemble());
            }
            _ => {
                // The page is written out and read back into another frame,
                // as when it leaves the buffer pool and returns.
                let image = view(&self.frame).to_image();
                TreePage::decode(&image, V1, self.size).expect("the image decodes");
                self.frame.fill(0xEE);
                self.frame[..image.len()].copy_from_slice(&image);
            }
        }
    }
}

#[test]
fn random_changes_keep_every_cell_under_its_slot() {
    for (seed, size, steps, check_every) in [
        (0x7EE1, PageSize::MIN, 12_000, 1),
        (0x7EE2, PageSize::MIN, 12_000, 1),
        (0x7EE3, PageSize::new(11_111).expect("in range"), 6_000, 3),
        (0x7EE4, PageSize::DEFAULT, 9_000, 7),
        (0x7EE5, PageSize::MAX, 9_000, 11),
    ] {
        let mut harness = Harness::new(seed, size);
        let (mut most_cells, mut fullest) = (0, usize::MAX);
        for step in 0..steps {
            harness.step();
            if step % check_every == 0 {
                harness.assert_matches();
            }
            most_cells = most_cells.max(harness.model.cells.len());
            fullest = fullest.min(harness.model.free_bytes(size));
        }
        harness.assert_matches();
        // The run filled its page, so writes were refused and cells moved.
        assert!(most_cells >= 15, "{most_cells} cells at most");
        assert!(fullest < 300, "{fullest} bytes free at least");
        assert!(harness.model.departures > 500);
    }
}

/// A full page of random cells in a frame, with its model.
fn full_page(seed: u64, size: PageSize) -> Harness {
    let mut harness = Harness::new(seed, size);
    for _ in 0..600 {
        harness.insert();
    }
    harness.assert_matches();
    assert!(harness.model.free_bytes(size) < 1_000);
    harness
}

fn differing(before: &[u8], after: &[u8]) -> Vec<usize> {
    assert_eq!(before.len(), after.len());
    (0..before.len())
        .filter(|&at| before[at] != after[at])
        .collect()
}

#[test]
fn any_stored_hash_can_be_read_or_overwritten_without_touching_the_value_bytes() {
    for (seed, size) in [0x4A51, 0x4A52, 0x4A53].into_iter().zip(SIZES) {
        let mut harness = full_page(seed, size);
        let slots: Vec<u16> = harness.model.cells.keys().copied().collect();
        let (mut overwritten, mut absent) = (0, 0);
        for raw in slots {
            let slot = slot(raw);
            for field in HashField::ALL {
                let page = view(&harness.frame);
                let cell = page.cell(slot).expect("a cell");
                let cell_at = page.cell_range(slot).expect("a cell");
                let (value_start, value_end) = cell.value_extent();
                let value_at = cell_at.start + value_start..cell_at.start + value_end;
                let before = harness.frame.clone();
                let new = harness.rng.hash();

                let Ok(hash_at) = cell.hash_at(field) else {
                    // The node has no child on that side, so no hash either.
                    let HashField::Child(side) = field else {
                        panic!("a node always has its own hashes");
                    };
                    assert_eq!(cell.hash(field), None);
                    assert_eq!(
                        harness.page().set_hash(slot, field, &new),
                        Err(TreePageError::NoSuchLink { slot, side }.into())
                    );
                    assert!(harness.frame == before);
                    absent += 1;
                    continue;
                };
                let hash_at = cell_at.start + hash_at..cell_at.start + hash_at + HASH_LEN;

                // Read: the hash is these 32 bytes, which are inside the
                // cell and are not value bytes.
                assert_eq!(
                    cell.hash(field).expect("stored").as_slice(),
                    &before[hash_at.clone()]
                );
                assert!(cell_at.start <= hash_at.start && hash_at.end <= value_at.start);

                // Overwrite: these 32 bytes change and no other byte of the
                // page does. The value is where it was, as it was.
                harness.page().set_hash(slot, field, &new).expect("stored");
                let changed = differing(&before, &harness.frame);
                assert!(changed.iter().all(|at| hash_at.contains(at)));
                assert_eq!(harness.frame[hash_at], new);
                assert_eq!(harness.frame[value_at.clone()], before[value_at.clone()]);
                let page = view(&harness.frame);
                assert_eq!(page.cell_range(slot), Some(cell_at));
                let cell = page.cell(slot).expect("a cell");
                assert_eq!(cell.hash(field), Some(&new));
                assert_eq!(cell.value_extent(), (value_start, value_end));

                let model = harness.model.cells.get_mut(&raw).expect("a cell");
                *model.hash_mut(field).expect("stored") = new;
                overwritten += 1;
            }
        }
        harness.assert_matches();
        assert!(overwritten > 60 && absent > 10);
    }
}

#[test]
fn a_change_of_the_same_size_is_made_where_the_cell_is() {
    let size = PageSize::DEFAULT;
    let mut model = ModelPage::new(9, 0);
    let mut cell = Rng::new(1).cell(page_id(9), 100);
    cell.feature = vec![1, 2, 3];
    cell.hosted = vec![None, Some(address(9, 7))];
    cell.value = ModelValue::Inline(vec![0x77; 12]);
    for side in Side::BOTH {
        *cell.link_mut(side) = Some(ModelLink {
            child: address(9, 2),
            hash: [5; 32],
            heights: (2, 1),
            child_key_len: 4,
            aggregate: vec![1; 9],
        });
    }
    model.cells.insert(1, ModelCell::leaf(b"before", b"it"));
    model.cells.insert(2, cell.clone());
    model.cells.insert(3, ModelCell::leaf(b"after", b"it"));
    let mut frame = frame_of(&model, size);

    // Each change, the bytes of the cell it may write, and the cell after.
    type Write = fn(&mut TreePageMut<'_>, &ModelCell) -> Result<(), FormatError>;
    let cell_at = view(&frame).cell_range(slot(2)).expect("a cell").start;
    let link_len = 42 + 9;
    let right = 72 + link_len;
    let hosted = right + link_len;
    let feature = hosted + 12;
    let value = feature + 3 + cell.key.len();
    let mut steps: Vec<(Write, Vec<usize>, ModelCell)> = Vec::new();
    let mut next = cell.clone();

    next.right.as_mut().expect("a link").child = address(0xAABB_CCDD, 0xEEFF);
    steps.push((
        |page, cell| {
            page.set_child(
                slot(2),
                Side::Right,
                cell.right.as_ref().expect("a link").child,
            )
        },
        (right..right + 6).collect(),
        next.clone(),
    ));
    next.fragment_root = !next.fragment_root;
    steps.push((
        |page, cell| page.set_fragment_root(slot(2), cell.fragment_root),
        vec![0],
        next.clone(),
    ));
    next.hosted[0] = Some(address(0x0102_0304, 0x0506));
    steps.push((
        |page, cell| page.set_hosted_root(slot(2), 0, cell.hosted[0]),
        (hosted..hosted + 6).collect(),
        next.clone(),
    ));
    next.hosted[1] = None;
    steps.push((
        |page, cell| page.set_hosted_root(slot(2), 1, cell.hosted[1]),
        (hosted + 6..hosted + 12).collect(),
        next.clone(),
    ));
    next.hosted = vec![Some(address(3, 3)), Some(address(4, 4))];
    steps.push((
        |page, cell| page.set_hosted_roots(slot(2), &cell.hosted),
        (hosted..hosted + 12).collect(),
        next.clone(),
    ));
    next.left = Some(ModelLink {
        child: address(21, 22),
        hash: [0xC3; 32],
        heights: (9, 8),
        child_key_len: 200,
        aggregate: vec![0xD4; 9],
    });
    steps.push((
        |page, cell| {
            page.set_link(
                slot(2),
                Side::Left,
                cell.left.as_ref().map(ModelLink::link).as_ref(),
            )
        },
        (72..72 + link_len).collect(),
        next.clone(),
    ));
    next.feature = vec![0xF0, 0xF1, 0xF2];
    steps.push((
        |page, cell| page.set_feature(slot(2), &cell.feature),
        (feature..feature + 3).collect(),
        next.clone(),
    ));
    next.value = ModelValue::Inline(vec![0x88; 12]);
    steps.push((
        |page, cell| page.set_value(slot(2), cell.value.value()),
        (value..value + 12).collect(),
        next.clone(),
    ));
    // A pointer takes the room of a 12-byte value: the flags and the value
    // length change with it.
    next.value = ModelValue::OutOfLine(pointer(0x0A0B_0C0D, 0x1122_3344_5566, 0x99AA_BBCC));
    steps.push((
        |page, cell| page.set_value(slot(2), cell.value.value()),
        (0..1).chain(4..8).chain(value..value + 12).collect(),
        next.clone(),
    ));
    // The cleaner moves the value: another pointer in place of the pointer.
    next.value = ModelValue::OutOfLine(pointer(0x0A0B_0C0E, 0x20, 0x99AA_BBCC));
    steps.push((
        |page, cell| page.set_value(slot(2), cell.value.value()),
        (value..value + 12).collect(),
        next.clone(),
    ));

    let places = |frame: &[u8]| {
        let page = view(frame);
        let cells: Vec<_> = (1..=3).map(|raw| page.cell_range(slot(raw))).collect();
        (cells, page.encoded_len(), page.unused_bytes())
    };
    let places_before = places(&frame);
    for (write, may_change, after) in steps {
        let before = frame.clone();
        write(&mut open(&mut frame, size), &after).expect("fits");
        let changed = differing(&before, &frame);
        assert!(!changed.is_empty());
        for at in changed {
            assert!(
                may_change.contains(&(at - cell_at)),
                "byte {} of the cell",
                at - cell_at
            );
        }
        model.cells.insert(2, after);
        model.assert_is(view(&frame), size);
        // No cell moved, and the page is as long as it was.
        assert_eq!(places(&frame), places_before);
    }
}

#[test]
fn a_cell_is_smaller_than_half_a_page_whatever_its_key_and_value() {
    for size in SIZES
        .into_iter()
        .chain([PageSize::new(8_193).expect("in range")])
    {
        let max = size.max_cell_len();

        // The largest cell there is with its value out of line: the longest
        // key, feature and aggregates, two children, 255 hosted roots. Three
        // of them fit in the smallest page, and no cell is as large as half
        // of a page.
        let longest = vec![0x5A; 255];
        let mut largest = ModelCell::leaf(&longest, b"");
        largest.feature = longest.clone();
        largest.hosted = vec![Some(address(4, 4)); 255];
        for side in Side::BOTH {
            *largest.link_mut(side) = Some(ModelLink {
                child: address(8, 8),
                hash: [3; 32],
                heights: (255, 255),
                child_key_len: 255,
                aggregate: longest.clone(),
            });
        }
        largest.value = ModelValue::OutOfLine(pointer(1, 20, u32::MAX));
        assert_eq!(largest.len(), TreeCell::MAX_OUT_OF_LINE_LEN);
        assert!(largest.len() <= max);
        let mut model = ModelPage::new(2, 0);
        let mut frame = frame_of(&model, size);
        for raw in 1..=3 {
            open(&mut frame, size)
                .insert(slot(raw), &largest.content())
                .expect("fits");
            model.cells.insert(raw, largest.clone());
        }
        model.assert_is(view(&frame), size);

        // Every key length Merk accepts, with a value of any length: the
        // value goes out of line and the cell fits.
        let mut frame = frame_of(&ModelPage::new(2, 0), size);
        for key_len in 0..=255 {
            let mut cell = largest.clone();
            cell.key = vec![0x6B; key_len];
            let mut page = open(&mut frame, size);
            page.insert(SlotId::MIN, &cell.content()).expect("fits");
            cell.assert_is(page.page().cell(SlotId::MIN).expect("a cell"));
            page.remove(SlotId::MIN).expect("occupied");
        }

        // A value stays in its cell up to the length the page size gives.
        // One byte more is refused, and the same value out of line fits.
        let mut harness = Harness::new(0xC7, size);
        for shape in [ModelCell::leaf(b"", b""), ModelCell::leaf(&longest, b""), {
            let mut shape = largest.clone();
            shape.value = ModelValue::Inline(Vec::new());
            shape
        }] {
            let raw = harness.model.lowest_free_slot();
            let longest_inline = size.max_inline_value_len(shape.len());
            assert_eq!(shape.len() + longest_inline, max);
            let mut cell = shape.clone();
            cell.value = ModelValue::Inline(vec![0x11; longest_inline + 1]);
            let before = harness.frame.clone();
            assert_eq!(
                harness.page().insert(slot(raw), &cell.content()),
                Err(TreePageError::CellTooLarge { len: max + 1, max }.into())
            );
            assert!(harness.frame == before);
            cell.value = ModelValue::Inline(vec![0x11; longest_inline]);
            harness.change(raw, cell.clone(), |page, cell| {
                page.insert(slot(raw), &cell.content())
            });
            assert!(harness.model.cells.contains_key(&raw));

            // The same through a change of the value alone.
            let before = harness.frame.clone();
            let longer = vec![0x22; longest_inline + 1];
            assert_eq!(
                harness
                    .page()
                    .set_value(slot(raw), CellValue::Inline(&longer)),
                Err(TreePageError::CellTooLarge { len: max + 1, max }.into())
            );
            assert!(harness.frame == before);
            cell.value = ModelValue::OutOfLine(pointer(7, 20, longer.len() as u32));
            harness.change(raw, cell, |page, cell| {
                page.set_value(slot(raw), cell.value.value())
            });
            harness.page().remove(slot(raw)).expect("occupied");
            harness.model.cells.remove(&raw);
            harness.model.departures += 1;
        }
        harness.assert_matches();
    }
}

#[test]
fn a_cell_too_large_for_a_smaller_page_size_is_refused_on_reading() {
    let mut model = ModelPage::new(3, 0);
    model.cells.insert(1, ModelCell::leaf(b"small", b"value"));
    model
        .cells
        .insert(2, ModelCell::leaf(b"large", &vec![0x33; 20_000]));
    let frame = frame_of(&model, PageSize::MAX);
    let image = view(&frame).to_image();
    model.assert_is(
        TreePage::decode(&image, V1, PageSize::MAX).expect("valid"),
        PageSize::MAX,
    );
    let too_large: FormatError = TreePageError::CellTooLarge {
        len: 72 + 5 + 20_000,
        max: 16_370,
    }
    .into();
    assert_eq!(
        TreePage::decode(&image, V1, PageSize::DEFAULT).expect_err("too large"),
        too_large
    );
    assert_eq!(
        view(&image)
            .check(PageSize::DEFAULT)
            .expect_err("too large"),
        too_large
    );
    let mut target = ModelPage::new(4, 0);
    let mut small = frame_of(&target, PageSize::DEFAULT);
    let before = small.clone();
    let cell = view(&image).cell(slot(2)).expect("a cell");
    let mut page = open(&mut small, PageSize::DEFAULT);
    assert_eq!(page.insert_encoded(slot(1), cell), Err(too_large));
    assert!(small == before);

    let mut page = open(&mut small, PageSize::DEFAULT);
    page.insert_encoded(slot(1), view(&image).cell(slot(1)).expect("a cell"))
        .expect("fits");
    let before = small.clone();
    let mut page = open(&mut small, PageSize::DEFAULT);
    assert_eq!(page.replace_encoded(slot(1), cell), Err(too_large));
    assert!(small == before);
    target.cells.insert(1, model.cells[&1].clone());
    target.assert_is(view(&small), PageSize::DEFAULT);
}

#[test]
fn a_full_page_refuses_a_write_and_takes_it_once_there_is_room() {
    let size = PageSize::MIN;
    let mut harness = Harness::new(0xF011, size);
    let cell = ModelCell::leaf(b"key", &[0x44; 925]);
    assert_eq!(cell.len(), 1_000);
    for raw in 1..=8 {
        harness.change(raw, cell.clone(), |page, cell| {
            page.insert(slot(raw), &cell.content())
        });
    }
    // 22 + 8 * 1002 bytes are used, and a ninth cell needs 1002.
    assert_eq!(harness.model.free_bytes(size), 154);
    harness.change(9, cell.clone(), |page, cell| {
        page.insert(slot(9), &cell.content())
    });
    assert!(!harness.model.cells.contains_key(&9));
    let mut grown = cell.clone();
    grown.value = ModelValue::Inline(vec![0x55; 925 + 155]);
    harness.change(4, grown.clone(), |page, cell| {
        page.replace(slot(4), &cell.content())
    });
    assert_eq!(harness.model.cells[&4], cell);
    harness.change(4, grown.clone(), |page, cell| {
        page.set_value(slot(4), cell.value.value())
    });
    assert_eq!(harness.model.cells[&4], cell);

    // A cell in the middle leaves. Its room is not at the end of the page,
    // and the ninth cell still fits: the cells are gathered for it.
    harness.page().remove(slot(3)).expect("occupied");
    harness.model.cells.remove(&3);
    harness.model.departures += 1;
    assert_eq!(view(&harness.frame).unused_bytes(), 1_000);
    harness.change(9, cell.clone(), |page, cell| {
        page.insert(slot(9), &cell.content())
    });
    assert!(harness.model.cells.contains_key(&9));
    assert_eq!(view(&harness.frame).unused_bytes(), 0);
    harness.assert_matches();

    // The same for a cell that grows where the end of the page has no room.
    harness.page().remove(slot(2)).expect("occupied");
    harness.model.cells.remove(&2);
    harness.model.departures += 1;
    grown.value = ModelValue::Inline(vec![0x55; 925 + 1_100]);
    harness.change(6, grown.clone(), |page, cell| {
        page.replace(slot(6), &cell.content())
    });
    assert_eq!(harness.model.cells[&6], grown);
    harness.assert_matches();
    assert_eq!(harness.model.lowest_free_slot(), 2);
}

#[test]
fn slot_numbers_stay_and_the_lowest_free_one_is_taken_next() {
    let size = PageSize::DEFAULT;
    let mut harness = Harness::new(0x5107, size);
    for raw in 1..=10 {
        let cell = ModelCell::leaf(&[raw as u8; 9], &[raw as u8; 30]);
        harness.change(raw, cell, |page, cell| {
            page.insert(slot(raw), &cell.content())
        });
    }
    harness.assert_matches();
    for raw in [3, 7, 10] {
        harness.page().remove(slot(raw)).expect("occupied");
        harness.model.cells.remove(&raw);
        harness.model.departures += 1;
    }
    harness.assert_matches();
    let page = view(&harness.frame);
    assert_eq!(page.departures(), 5 + 3);
    assert_eq!(page.slot_count(), 9);
    assert_eq!(page.lowest_free_slot(), slot(3));
    for raw in [1, 2, 4, 5, 6, 8, 9] {
        assert_eq!(page.cell(slot(raw)).expect("a cell").key(), [raw as u8; 9]);
    }
    for expected in [3, 7, 10, 11] {
        let raw = view(&harness.frame).lowest_free_slot().get();
        assert_eq!(raw, expected);
        let cell = ModelCell::leaf(b"new", b"cell");
        harness.change(raw, cell, |page, cell| {
            page.insert(slot(raw), &cell.content())
        });
    }
    harness.assert_matches();

    // A page that loses its last cell is an empty page that keeps its
    // count of departures.
    let slots: Vec<u16> = harness.model.cells.keys().copied().collect();
    for raw in slots {
        harness.page().remove(slot(raw)).expect("occupied");
        harness.model.cells.remove(&raw);
        harness.model.departures += 1;
        harness.assert_matches();
    }
    let page = view(&harness.frame);
    assert_eq!(page.departures(), 5 + 3 + 11);
    assert_eq!((page.encoded_len(), page.slot_count()), (22, 0));
    assert_eq!(page.unused_bytes(), 0);
    assert_eq!(page.lowest_free_slot(), SlotId::MIN);
}

#[test]
fn the_count_of_departures_wraps_and_never_stops() {
    let size = PageSize::MIN;
    let mut frame = vec![0; size.get()];
    let mut page = TreePageMut::init(&mut frame, size, page_id(1), u64::MAX).expect("fits");
    let cell = ModelCell::leaf(b"k", b"v");
    page.insert(slot(1), &cell.content()).expect("fits");
    assert_eq!(page.page().departures(), u64::MAX);
    page.remove(slot(1)).expect("occupied");
    assert_eq!(page.page().departures(), 0);
}

#[test]
fn the_directory_grows_past_the_cells_in_its_way() {
    let size = PageSize::MIN;
    let mut harness = Harness::new(0xD1, size);
    for raw in 1..=5 {
        let cell = ModelCell::leaf(&[raw as u8], &[raw as u8; 1_100]);
        harness.change(raw, cell, |page, cell| {
            page.insert(slot(raw), &cell.content())
        });
    }
    // The page as it comes from a segment: its first cell starts where the
    // directory ends.
    let image = view(&harness.frame).to_image();
    harness.frame.fill(0);
    harness.frame[..image.len()].copy_from_slice(&image);
    assert_eq!(view(&harness.frame).unused_bytes(), 0);

    // A new highest slot moves that cell to the end of the page.
    let small = ModelCell::leaf(b"s", b"");
    harness.change(6, small.clone(), |page, cell| {
        page.insert(slot(6), &cell.content())
    });
    assert!(harness.model.cells.contains_key(&6));
    assert_eq!(view(&harness.frame).unused_bytes(), 1_173 - 2);
    harness.assert_matches();

    // A slot far up takes a directory that reaches over several cells.
    harness.change(700, small.clone(), |page, cell| {
        page.insert(slot(700), &cell.content())
    });
    assert!(harness.model.cells.contains_key(&700));
    assert_eq!(view(&harness.frame).slot_count(), 700);
    harness.assert_matches();

    // One that does not fit is refused.
    harness.change(4_000, small.clone(), |page, cell| {
        page.insert(slot(4_000), &cell.content())
    });
    assert!(!harness.model.cells.contains_key(&4_000));
    let before = harness.frame.clone();
    assert_eq!(
        harness.page().insert(SlotId::MAX, &small.content()),
        Err(TreePageError::PageFull {
            needed: 73 + 2 * (65_535 - 700),
            available: harness.model.free_bytes(size),
        }
        .into())
    );
    assert!(harness.frame == before);
}

#[test]
fn the_directory_grows_in_a_page_with_no_room_at_its_end() {
    let size = PageSize::MIN;
    let mut harness = Harness::new(0xD2, size);
    let cell = ModelCell::leaf(b"key", &[0x44; 925]);
    for raw in 1..=8 {
        harness.change(raw, cell.clone(), |page, cell| {
            page.insert(slot(raw), &cell.content())
        });
    }
    let image = view(&harness.frame).to_image();
    harness.frame[..image.len()].copy_from_slice(&image);
    // The room is in the middle. The end of the page has 154 bytes, less
    // than the cell that is in the way of the directory.
    harness.page().remove(slot(5)).expect("occupied");
    harness.model.cells.remove(&5);
    harness.model.departures += 1;
    assert_eq!(view(&harness.frame).unused_bytes(), 1_000);
    harness.change(9, cell.clone(), |page, cell| {
        page.insert(slot(9), &cell.content())
    });
    assert!(harness.model.cells.contains_key(&9));
    harness.assert_matches();
    assert_eq!(view(&harness.frame).unused_bytes(), 0);
    assert_eq!(view(&harness.frame).lowest_free_slot(), slot(5));
}

#[test]
fn a_cell_is_carried_to_another_page_as_its_bytes() {
    let size = PageSize::DEFAULT;
    let mut rng = Rng::new(0xCA44);
    let mut from = Harness::new(1, size);
    let mut to = Harness::new(2, size);
    to.model.id = page_id(99);
    to.frame = frame_of(&to.model, size);
    for raw in 1..=20 {
        let cell = rng.cell(from.model.id, 300);
        from.change(raw, cell, |page, cell| {
            page.insert(slot(raw), &cell.content())
        });
    }
    for raw in (1..=20).rev().step_by(2) {
        let source = from.frame.clone();
        let cell = view(&source).cell(slot(raw)).expect("a cell");
        let target = to.model.lowest_free_slot();
        to.page().insert_encoded(slot(target), cell).expect("fits");
        from.page().remove(slot(raw)).expect("occupied");
        assert_eq!(
            view(&to.frame)
                .cell(slot(target))
                .expect("a cell")
                .as_bytes(),
            cell.as_bytes()
        );
        let carried = from.model.cells.remove(&raw).expect("a cell");
        from.model.departures += 1;
        to.model.cells.insert(target, carried);
    }
    from.assert_matches();
    to.assert_matches();
    assert_eq!(view(&from.frame).departures(), 5 + 10);
    assert_eq!(view(&to.frame).departures(), 5);
}

#[test]
fn a_cell_that_would_not_decode_is_not_taken() {
    // A cell with half a hosted-root pointer, read from a page that was not
    // checked.
    let mut model = ModelPage::new(3, 0);
    let mut cell = ModelCell::leaf(b"k", b"v");
    cell.hosted = vec![Some(address(1, 1))];
    model.cells.insert(1, cell);
    let mut image = model.assemble();
    image[24 + 72] = 0;
    let damaged = view(&image).cell(slot(1)).expect("a cell by its lengths");
    assert_eq!(damaged.hosted_roots().collect::<Vec<_>>(), [None]);

    let size = PageSize::DEFAULT;
    let mut frame = frame_of(&example(), size);
    let before = frame.clone();
    let mut page = open(&mut frame, size);
    assert_eq!(
        page.insert_encoded(slot(2), damaged),
        Err(FormatError::MissingId)
    );
    assert_eq!(
        page.replace_encoded(slot(1), damaged),
        Err(FormatError::MissingId)
    );
    assert_eq!(frame, before);
}

#[test]
fn a_write_to_a_free_slot_or_a_missing_part_is_refused() {
    let size = PageSize::DEFAULT;
    let mut frame = frame_of(&example(), size);
    let before = frame.clone();
    let mut page = open(&mut frame, size);
    let cell = ModelCell::leaf(b"k", b"v");
    let link = ModelLink {
        child: address(1, 1),
        hash: [0; 32],
        heights: (0, 0),
        child_key_len: 0,
        aggregate: Vec::new(),
    };

    assert_eq!(
        page.insert(slot(1), &cell.content()),
        Err(TreePageError::SlotOccupied { slot: slot(1) }.into())
    );
    for free in [2, 4, 5_000, u16::MAX] {
        let free = slot(free);
        let refused = Err(TreePageError::SlotFree { slot: free }.into());
        assert_eq!(page.remove(free), refused);
        assert_eq!(page.replace(free, &cell.content()), refused);
        assert_eq!(page.set_hash(free, HashField::Value, &[1; 32]), refused);
        assert_eq!(page.set_child(free, Side::Left, address(1, 1)), refused);
        assert_eq!(page.set_link(free, Side::Left, None), refused);
        assert_eq!(page.set_link(free, Side::Left, Some(&link.link())), refused);
        assert_eq!(page.set_fragment_root(free, true), refused);
        assert_eq!(page.set_hosted_root(free, 0, None), refused);
        assert_eq!(page.set_hosted_roots(free, &[]), refused);
        assert_eq!(page.set_feature(free, &[]), refused);
        assert_eq!(page.set_value(free, CellValue::Inline(&[])), refused);
    }

    // The cell of slot 1 has a left child and two hosted roots. The cell of
    // slot 3 has neither.
    let no_link = |slot, side| Err(TreePageError::NoSuchLink { slot, side }.into());
    assert_eq!(
        page.set_hash(slot(1), HashField::Child(Side::Right), &[1; 32]),
        no_link(slot(1), Side::Right)
    );
    assert_eq!(
        page.set_child(slot(1), Side::Right, address(1, 1)),
        no_link(slot(1), Side::Right)
    );
    assert_eq!(
        page.set_child(slot(3), Side::Left, address(1, 1)),
        no_link(slot(3), Side::Left)
    );
    assert_eq!(
        page.set_hosted_root(slot(1), 2, None),
        Err(TreePageError::NoSuchHostedRoot {
            slot: slot(1),
            index: 2
        }
        .into())
    );
    assert_eq!(
        page.set_hosted_root(slot(3), 0, Some(address(1, 1))),
        Err(TreePageError::NoSuchHostedRoot {
            slot: slot(3),
            index: 0
        }
        .into())
    );
    // Taking away a link that is not there changes nothing.
    page.set_link(slot(3), Side::Left, None).expect("occupied");
    page.set_link(slot(1), Side::Right, None).expect("occupied");
    assert!(frame == before);
}

#[test]
fn a_page_is_opened_in_a_buffer_of_its_page_size() {
    for size in SIZES {
        let needed = size.image_capacity();
        let too_small: FormatError = TreePageError::BufferTooSmall {
            len: needed - 1,
            needed,
        }
        .into();
        let mut frame = frame_of(&example(), size);
        assert_eq!(
            TreePageMut::init(&mut frame[..needed - 1], size, page_id(1), 0).expect_err("small"),
            too_small
        );
        assert_eq!(
            TreePageMut::open(&mut frame[..needed - 1], size, V1).expect_err("small"),
            too_small
        );
        assert_eq!(
            TreePageMut::open(&mut [], size, V1).expect_err("small"),
            TreePageError::BufferTooSmall { len: 0, needed }.into()
        );
        // A frame of 64 KiB has one byte a page cannot use.
        let page = TreePageMut::open(&mut frame[..needed], size, V1).expect("a page");
        assert_eq!(page.free_bytes(), needed - 247);
        example().assert_is(page.page(), size);
        assert!(format!("{page:?}").contains("TreePageMut"));

        // What is in the buffer must be a page.
        frame[0] = 1;
        assert_eq!(
            TreePageMut::open(&mut frame, size, V1).expect_err("a flat leaf"),
            FormatError::UnknownRecordKind { code: 1 }
        );
    }
}

#[test]
fn gathering_the_unused_bytes_gives_the_image() {
    let mut harness = full_page(0xC0A7, PageSize::DEFAULT);
    for _ in 0..40 {
        harness.remove();
    }
    let unused = view(&harness.frame).unused_bytes();
    assert!(unused > 1_000);
    let image = view(&harness.frame).to_image();
    let free = harness.page().free_bytes();
    harness.page().compact().expect("a valid page");
    let page = view(&harness.frame);
    assert_eq!(page.unused_bytes(), 0);
    assert_eq!(page.as_bytes(), image);
    assert_eq!(page.free_bytes(harness.size), free);
    TreePage::decode(&harness.frame, V1, harness.size).expect("the frame starts with the image");
    harness.assert_matches();

    // Gathering again changes nothing.
    let before = harness.frame.clone();
    harness.page().compact().expect("a valid page");
    assert!(harness.frame == before);
}

#[test]
fn a_damaged_page_is_refused_where_the_damage_is_met() {
    // `TreePageMut` needs a valid page. On one whose slot leads nowhere it
    // refuses what touches that slot, and what has to move its cell.
    let size = PageSize::MIN;
    let mut model = ModelPage::new(3, 0);
    for raw in 1..=7 {
        model
            .cells
            .insert(raw, ModelCell::leaf(&[raw as u8], &[raw as u8; 1_000]));
    }
    let image = model.assemble();
    let damaged = |offset: u16| {
        let mut frame = image.clone();
        frame.resize(size.get(), 0);
        frame[22 + 2 * 3..22 + 2 * 4].copy_from_slice(&offset.to_le_bytes());
        frame
    };
    let invalid: FormatError = TreePageError::InvalidSlot { slot: slot(4) }.into();
    let cell = ModelCell::leaf(b"new", &[0; 1_000]);
    for offset in [7, 8_000, 60_000] {
        let mut frame = damaged(offset);
        let mut page = open(&mut frame, size);
        assert_eq!(page.remove(slot(4)), Err(invalid));
        assert_eq!(
            page.set_hash(slot(4), HashField::Value, &[0; 32]),
            Err(invalid)
        );
        assert_eq!(page.replace(slot(4), &cell.content()), Err(invalid));
        // Gathering the cells meets the slot, and so does a new cell that
        // needs the room the page has only once they are gathered.
        assert_eq!(page.compact(), Err(invalid));
        let mut frame = damaged(offset);
        let mut page = open(&mut frame, size);
        page.remove(slot(2)).expect("a valid cell");
        assert_eq!(page.insert(slot(8), &cell.content()), Err(invalid));
    }
    // A slot that leads into the directory is in the way of a new slot.
    let mut frame = damaged(23);
    let mut page = open(&mut frame, size);
    assert_eq!(
        page.insert(slot(8), &ModelCell::leaf(b"", b"").content()),
        Err(invalid)
    );
}

#[test]
fn a_cell_is_replaced_only_by_a_cell_of_the_same_node() {
    let size = PageSize::DEFAULT;
    let mut frame = frame_of(&example(), size);
    let before = frame.clone();
    let mut page = open(&mut frame, size);
    let departures = page.page().departures();

    // A slot names one node until its cell leaves the page. A cell with
    // another key is another node, and is refused.
    let refused = Err(TreePageError::KeyChanged { slot: slot(3) }.into());
    for key in [&b"ab"[..], b"abcd", b"abd", b""] {
        let other = ModelCell::leaf(key, b"v");
        assert_eq!(page.replace(slot(3), &other.content()), refused);
        let bytes = other.assemble();
        let other = TreeCell::decode(&bytes, V1).expect("valid");
        assert_eq!(page.replace_encoded(slot(3), other), refused);
    }
    assert!(frame == before);

    // The same key with everything else changed is the same node.
    let mut page = open(&mut frame, size);
    let mut same = example().cells[&1].clone();
    same.key = b"abc".to_vec();
    same.value = ModelValue::Inline(vec![7; 500]);
    page.replace(slot(3), &same.content())
        .expect("the same key");
    assert_eq!(page.page().departures(), departures);
    let mut model = example();
    model.cells.insert(3, same);
    model.assert_is(view(&frame), size);
}

#[test]
fn every_aggregate_fits_a_link_and_is_rewritten_where_it_is() {
    // What a feature or an aggregate takes when it is Merk's tag followed by
    // each count and sum at its full width: no number, one of 8 bytes, a
    // 128-bit sum or two of 8 bytes, and a count with a 128-bit sum, which is
    // the widest Merk has. Then a wider one that Merk does not have yet, and
    // the widest the format takes.
    let size = PageSize::DEFAULT;
    for width in [1, 9, 17, 25, 41, 255] {
        let link = |seed: u8| ModelLink {
            child: address(9, 2),
            hash: [seed; 32],
            heights: (seed, seed),
            child_key_len: seed,
            aggregate: vec![seed; width],
        };
        let mut cell = ModelCell::leaf(b"key", b"value");
        cell.feature = vec![1; width];
        cell.left = Some(link(2));
        cell.right = Some(link(3));
        assert_eq!(
            cell.left.as_ref().expect("a link").link().encoded_len(),
            42 + width
        );
        assert_eq!(cell.len(), 72 + 3 * width + 2 * 42 + 3 + 5);

        let mut model = ModelPage::new(9, 0);
        model.cells.insert(1, ModelCell::leaf(b"before", b"it"));
        model.cells.insert(2, cell.clone());
        model.cells.insert(3, ModelCell::leaf(b"after", b"it"));
        let mut frame = frame_of(&model, size);
        let cell_at = view(&frame).cell_range(slot(2)).expect("a cell");

        // New numbers in the feature and in both aggregates, with new hashes
        // and heights, as a write below this node gives them: every byte
        // that changes is in its link or in the feature, and the cell is
        // where it was and as long as it was.
        let before = frame.clone();
        cell.feature = vec![0xF1; width];
        cell.left = Some(link(0xA2));
        cell.right = Some(link(0xA3));
        let mut page = open(&mut frame, size);
        page.set_feature(slot(2), &cell.feature).expect("fits");
        for side in Side::BOTH {
            let link = cell.link(side).as_ref().map(ModelLink::link);
            page.set_link(slot(2), side, link.as_ref()).expect("fits");
        }
        let links_and_feature = cell_at.start + 72..cell_at.start + 72 + 2 * (42 + width) + width;
        let changed = differing(&before, &frame);
        assert!(!changed.is_empty());
        assert!(changed.iter().all(|at| links_and_feature.contains(at)));
        assert_eq!(view(&frame).cell_range(slot(2)), Some(cell_at));
        model.cells.insert(2, cell);
        model.assert_is(view(&frame), size);
    }
    // A link with the widest aggregate Merk has is 67 bytes.
    assert_eq!(TreeLink::FIXED_LEN + 25, 67);
    assert_eq!(TreeLink::MAX_LEN, 42 + 255);
}
