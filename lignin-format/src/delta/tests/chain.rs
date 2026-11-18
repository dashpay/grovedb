//! A base image and a chain of runs against the page they describe.

use super::*;

/// One page as four parties see it.
///
/// - The writer changes its frame through `TreePageMut`, as the tree layer
///   does, and never sees a record.
/// - The model owns the page's parts and lays out its image by hand.
/// - The replica's frame takes each change as a record that was written and
///   read back, at once.
/// - The chain is what a store keeps: a base image and the runs since.
struct Chain {
    size: TreePageSize,
    rng: Rng,
    model: ModelPage,
    writer: Vec<u8>,
    replica: Vec<u8>,
    base: Option<Vec<u8>>,
    runs: Vec<Vec<u8>>,
    creates: Option<u64>,
    open: Vec<Op>,
    written: [usize; Op::KINDS],
    refused: usize,
    creations: usize,
    consolidations: usize,
    most_runs: usize,
    fullest: usize,
}

impl Chain {
    /// A chain for a page that exists, with a base image, or for one that
    /// its first run creates.
    fn new(seed: u64, size: TreePageSize, with_base: bool) -> Self {
        let mut rng = Rng::new(seed);
        let mut model = ModelPage::new(17, 5);
        if with_base {
            for raw in 1..=12 {
                model.cells.insert(raw, rng.cell(model.id, 200));
            }
        }
        let writer = frame_of(&model, size);
        Self {
            size,
            rng,
            base: with_base.then(|| model.assemble()),
            creates: (!with_base).then_some(model.departures),
            // The replica of a page that is yet to be created holds nothing
            // of it.
            replica: match with_base {
                true => writer.clone(),
                false => vec![0xEE; size.get()],
            },
            writer,
            model,
            runs: Vec::new(),
            open: Vec::new(),
            written: [0; Op::KINDS],
            refused: 0,
            creations: 0,
            consolidations: 0,
            most_runs: 0,
            fullest: usize::MAX,
        }
    }

    /// Whether the replica has the page: it lacks a page that the run being
    /// written is yet to create.
    fn replica_has_page(&self) -> bool {
        self.creates.is_none() || !self.open.is_empty()
    }

    /// Ends the run that is being written, if it holds a record.
    fn close_run(&mut self) {
        if self.creates.is_none() && self.open.is_empty() {
            return;
        }
        let has_page = self.replica_has_page();
        let bytes = run_of(self.model.id, self.creates.take(), &self.open);
        // The replica takes the records one at a time. A run that only
        // creates has no record of a change, and reaches it here.
        if !has_page {
            decode(&bytes)
                .apply(&mut self.replica, self.size, V1)
                .expect("creates");
        }
        self.open.clear();
        self.runs.push(bytes);
        self.most_runs = self.most_runs.max(self.runs.len());
    }

    /// The page is freed and made again under its id: the next run creates
    /// it.
    fn create_again(&mut self) {
        self.close_run();
        let departures = self.rng.below(1000) as u64;
        self.model = ModelPage::new(self.model.id.get(), departures);
        TreePageMut::init(&mut self.writer, self.size, self.model.id, departures).expect("fits");
        self.creates = Some(departures);
        self.creations += 1;
    }

    /// One change: the writer makes it, and if the page takes it, it is
    /// written as a record.
    fn step(&mut self) {
        let op = Op::random(&mut self.rng, &self.model, self.size);
        let before = self.writer.clone();
        let result = op.write(&mut open(&mut self.writer, self.size));

        if let Err(error) = op.with_delta(|delta| delta.encode_into(&mut Vec::new())) {
            // Only a cell or a value that no page takes has no record.
            assert!(matches!(
                error,
                FormatError::TreePage(
                    TreePageError::CellTooLarge { .. }
                        | TreePageError::FieldTooLong {
                            field: CellField::InlineValue,
                            ..
                        }
                )
            ));
            assert!(matches!(
                result,
                Err(FormatError::TreePage(TreePageError::CellTooLarge { .. }))
            ));
            assert!(self.writer == before);
            self.refused += 1;
            return;
        }
        // The record of the change, alone in a run, written and read back.
        // The first record for a page that is yet to be created comes
        // behind the record that creates it.
        let has_page = self.replica_has_page();
        let created = self.creates.filter(|_| !has_page);
        let bytes = run_of(self.model.id, created, std::slice::from_ref(&op));
        let run = decode(&bytes);
        assert_eq!(run.deltas().len(), 1);
        op.with_delta(|delta| assert_eq!(run.deltas().next(), Some(delta)));

        match result {
            Ok(()) => {
                run.apply(&mut self.replica, self.size, V1)
                    .expect("the replica takes what the writer took");
                op.make(&mut self.model);
                self.written[op.kind()] += 1;
                self.open.push(op);
            }
            Err(error) => {
                assert!(self.writer == before, "a refused write changed the page");
                // A change the writer's page refuses is refused as a record
                // too, with the same error, and changes nothing. A page
                // that is yet to be created has no replica to refuse it.
                if has_page {
                    let replica = self.replica.clone();
                    assert_eq!(run.apply(&mut self.replica, self.size, V1), Err(error));
                    assert!(self.replica == replica);
                }
                // And by the same page with its cells laid out another
                // way, as its image has them: a page takes or refuses a
                // change by its content.
                let image = image_of(&self.writer);
                let mut compact = vec![0xC3; self.size.get()];
                compact[..image.len()].copy_from_slice(&image);
                assert_eq!(run.apply(&mut compact, self.size, V1), Err(error));
                assert_eq!(image_of(&compact), image);
                self.refused += 1;
            }
        }
        // The same calls on equal frames: the replica is the writer's
        // frame, byte for byte.
        if self.replica_has_page() {
            assert!(
                self.writer == self.replica,
                "the replica is not the writer's frame"
            );
        }
        self.fullest = self.fullest.min(self.model.free_bytes(self.size));
    }

    /// The image `base` and `runs` give, by consolidation.
    fn consolidated(&self, base: Option<&[u8]>, runs: &[Vec<u8>]) -> Vec<u8> {
        let runs: Vec<DeltaRun<'_>> = runs.iter().map(|bytes| decode(bytes)).collect();
        let mut out = vec![0x3C; 5];
        consolidate(self.model.id, base, &runs, V1, self.size, &mut out).expect("consolidates");
        assert_eq!(out[..5], [0x3C; 5]);
        out.split_off(5)
    }

    /// Checks the chain against the writer and the model, in every way a
    /// store can put it together.
    fn check(&mut self) {
        self.close_run();
        let expected = self.model.assemble();
        assert_eq!(image_of(&self.writer), expected);
        assert_eq!(image_of(&self.replica), expected);
        let page = TreePage::decode(&expected, V1, self.size).expect("an image");
        self.model.assert_is(page, self.size);

        // The acceptance test: the base image and the chain are the
        // consolidated image.
        let base = self.base.as_deref();
        assert_eq!(self.consolidated(base, &self.runs), expected);

        // The base image in a frame, and the chain applied to it there.
        let mut frame = vec![0xC3; self.size.get()];
        if let Some(base) = base {
            frame[..base.len()].copy_from_slice(base);
        }
        let runs: Vec<DeltaRun<'_>> = self.runs.iter().map(|bytes| decode(bytes)).collect();
        apply_chain(self.model.id, &mut frame, self.size, V1, &runs).expect("applies");
        assert_eq!(image_of(&frame), expected);

        // Consolidated at any point on the way, and again from there.
        if !self.runs.is_empty() {
            let cut = self.rng.below(self.runs.len() + 1);
            let (older, newer) = self.runs.split_at(cut);
            let middle = match (base, cut) {
                (None, 0) => None,
                _ => Some(self.consolidated(base, older)),
            };
            assert_eq!(self.consolidated(middle.as_deref(), newer), expected);
        }

        // The runs since the page was last created, folded into one: what
        // one write for the changes of several commits holds.
        let (base, tail) = match runs.iter().rposition(|run| run.created().is_some()) {
            Some(creating) => (None, &runs[creating..]),
            None => (base, &runs[..]),
        };
        if let Some(first) = tail.first() {
            let mut folded = Vec::new();
            let mut writer = DeltaRunWriter::new(self.model.id, first.created(), &mut folded);
            for delta in tail.iter().flat_map(DeltaRun::deltas) {
                writer.push(&delta).expect("encodes");
            }
            writer.finish().expect("a run");
            assert_eq!(self.consolidated(base, &[folded]), expected);
        }
    }

    /// The store writes a new base image and drops the chain.
    fn consolidate(&mut self) {
        self.check();
        self.base = Some(self.consolidated(self.base.as_deref(), &self.runs));
        self.runs.clear();
        self.consolidations += 1;
    }
}

/// The acceptance test of the task: a base image plus any delta chain
/// equals the consolidated image.
///
/// "Any chain" is every change `TreePageMut` can make, drawn at random,
/// cut into runs at random places, on pages of four sizes that fill up, are
/// emptied, and are freed and created again.
#[test]
fn a_base_image_plus_any_delta_chain_is_the_consolidated_image() {
    for (seed, size, with_base, steps) in [
        (0xDE17A1, TreePageSize::MIN, true, 9_000),
        (0xDE17A2, TreePageSize::MIN, false, 9_000),
        (
            0xDE17A3,
            TreePageSize::new(11_111).expect("in range"),
            true,
            6_000,
        ),
        (0xDE17A4, TreePageSize::DEFAULT, false, 9_000),
        (0xDE17A5, TreePageSize::MAX, true, 9_000),
    ] {
        let mut chain = Chain::new(seed, size, with_base);
        // The page is freed and created again five times in the first part
        // of the run, and then left to fill up.
        let eighth = steps / 8;
        for step in 1..=steps {
            chain.step();
            if step % eighth == 0 && step / eighth <= 5 {
                chain.create_again();
            }
            match chain.rng.below(1000) {
                0..80 => chain.close_run(),
                80..100 => chain.check(),
                100..108 => chain.consolidate(),
                _ => {}
            }
        }
        chain.check();

        // The run made every kind of change, filled its page so that
        // changes were refused, and kept long chains.
        for (kind, &count) in chain.written.iter().enumerate() {
            assert!(count >= 20, "{count} changes of kind {kind}");
        }
        assert!(chain.refused >= 200, "{} refused", chain.refused);
        assert!(chain.fullest < 300, "{} bytes free at least", chain.fullest);
        assert!(chain.model.departures > 0);
        assert_eq!(chain.creations, 5);
        assert!(
            chain.consolidations >= 20,
            "{} consolidations",
            chain.consolidations
        );
        assert!(chain.most_runs >= 15, "{} runs at most", chain.most_runs);
    }
}

/// A chain is a function of the changes, not of how they were cut into
/// runs or of the bytes the writer's frame happened to have.
#[test]
fn the_same_changes_give_the_same_image_however_they_are_cut() {
    let size = TreePageSize::MIN;
    let mut rng = Rng::new(0xC075);
    let mut model = ModelPage::new(3, 0);
    for raw in 1..=8 {
        model.cells.insert(raw, rng.cell(model.id, 200));
    }
    let base = model.assemble();
    let mut frame = frame_of(&model, size);
    let mut ops = Vec::new();
    while ops.len() < 400 {
        let op = Op::random(&mut rng, &model, size);
        if op.write(&mut open(&mut frame, size)).is_ok() {
            op.make(&mut model);
            ops.push(op);
        }
    }
    let expected = model.assemble();

    for pieces in [1, 2, 7, 400] {
        let runs: Vec<Vec<u8>> = ops
            .chunks(ops.len().div_ceil(pieces))
            .map(|ops| run_of(model.id, None, ops))
            .collect();
        let runs: Vec<DeltaRun<'_>> = runs.iter().map(|bytes| decode(bytes)).collect();
        assert_eq!(runs.len(), pieces);
        let mut image = Vec::new();
        consolidate(model.id, Some(&base), &runs, V1, size, &mut image).expect("consolidates");
        assert_eq!(image, expected);

        // The same in a frame that is larger than the image and holds
        // other bytes behind it.
        for filler in [0x00, 0xFF] {
            let mut frame = vec![filler; size.get() + 100];
            frame[..base.len()].copy_from_slice(&base);
            apply_chain(model.id, &mut frame, size, V1, &runs).expect("applies");
            assert_eq!(image_of(&frame), expected);
        }
    }
}
