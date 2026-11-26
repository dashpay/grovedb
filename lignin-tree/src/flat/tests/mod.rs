mod cursor;
mod errors;
mod keys;
mod log;
mod many;
mod model;
mod moves;
mod shape;
mod steps;
pub(crate) mod store;

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use lignin_format::{
    flat_page::{common_prefix_len, FlatPage, FlatValue},
    value_log::{PendingValue, ValuePlace, ValuePointer},
    wal::{FlatRun, FlatRunWriter, ValueMove},
    FileId, FormatVersion, PageId, PageSize,
};
use lignin_io::sim::SimRng;

use super::{
    leaf_images_written, leaf_pieces_cut,
    log::{is_changed, push_changes},
    Changes, Cursor, FlatConfig, FlatError, FlatPages, FlatPagesMut, FlatTree, Look, Revision,
    Written,
};

const V1: FormatVersion = FormatVersion::V1;

fn page_id(raw: u32) -> PageId {
    PageId::new(raw).expect("not zero")
}

fn small() -> FlatConfig {
    FlatConfig::new(PageSize::MIN)
}

/// The place of an out-of-line value of a page.
fn place_of(bytes: &[u8]) -> ValuePlace {
    ValuePlace::decode(bytes.try_into().expect("16 bytes")).expect("a place")
}

/// One move of an entry with a value out of line from a page to another.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Carry {
    from: PageId,
    to: PageId,
    place: ValuePlace,
    held: PendingValue,
}

/// What one commit wrote to the log: the run of each page it changed, and
/// the pages it freed.
#[derive(Debug, Default)]
struct Commit {
    runs: Vec<(PageId, Vec<u8>)>,
    freed: Vec<PageId>,
}

/// The pages of a flat keyspace in memory, with the value log and the
/// commits of a store reduced to what the tree can see of them. It checks
/// what the tree promises an implementation of [`FlatPagesMut`].
struct Mem {
    config: FlatConfig,
    pages: BTreeMap<PageId, Vec<u8>>,
    /// The pages as the last commit left them.
    base: BTreeMap<PageId, Vec<u8>>,
    /// Pending values, by their page and the bytes that name them.
    pending: HashMap<(PageId, [u8; 8]), Vec<u8>>,
    /// Every value that was ever given a pointer, with its owner.
    value_log: HashMap<ValuePointer, (PageId, Vec<u8>)>,
    /// The pointers that an entry holds.
    live: HashSet<ValuePointer>,
    taken: BTreeSet<PageId>,
    freed: Vec<PageId>,
    released: Vec<ValuePointer>,
    carried: Vec<Carry>,
    held: Vec<(PageId, PendingValue)>,
    /// What each commit wrote to the log.
    log: Vec<Commit>,
    next_page: u32,
    next_value: u64,
    next_offset: u64,
    revision: u64,
    /// How many images of leaves were written, and how many pages were
    /// taken.
    leaf_writes: u64,
    takes: u64,
    /// The page of each image of a leaf that was written, in order.
    leaf_pages: Vec<PageId>,
    /// A step of background work that is made just before the page is
    /// taken: it gives the image the page has from then on.
    step_before_take: Option<(PageId, Vec<u8>)>,
}

impl Mem {
    fn new(config: FlatConfig) -> Self {
        Self {
            config,
            pages: BTreeMap::new(),
            base: BTreeMap::new(),
            pending: HashMap::new(),
            value_log: HashMap::new(),
            live: HashSet::new(),
            taken: BTreeSet::new(),
            freed: Vec::new(),
            released: Vec::new(),
            carried: Vec::new(),
            held: Vec::new(),
            log: Vec::new(),
            next_page: 0,
            next_value: 0,
            next_offset: 20,
            revision: 0,
            leaf_writes: 0,
            takes: 0,
            leaf_pages: Vec::new(),
            step_before_take: None,
        }
    }

    fn tree(config: FlatConfig) -> (Self, FlatTree) {
        let mut pages = Self::new(config);
        let tree = FlatTree::create(&mut pages, config).expect("an empty keyspace");
        (pages, tree)
    }

    fn image(&self, page: PageId) -> FlatPage<'_> {
        FlatPage::decode(&self.pages[&page], V1).expect("a page")
    }

    /// The out-of-line values of the entries of `page`, by position.
    fn places(&self, page: PageId) -> Vec<(usize, ValuePlace)> {
        Self::places_of(&self.pages[&page])
    }

    fn places_of(image: &[u8]) -> Vec<(usize, ValuePlace)> {
        let Some(leaf) = FlatPage::decode(image, V1).expect("a page").leaf() else {
            return Vec::new();
        };
        leaf.iter()
            .enumerate()
            .filter_map(|(slot, (_, value))| match value {
                FlatValue::Inline(_) => None,
                FlatValue::OutOfLine(place) => Some((slot, place_of(place))),
            })
            .collect()
    }

    /// Write-back: every pending value gets a pointer into the value log,
    /// with the page of its entry as the owner, in a page step.
    fn place_values(&mut self) {
        let pages: Vec<PageId> = self.pages.keys().copied().collect();
        for page in pages {
            for (_, place) in self.places(page) {
                let ValuePlace::Pending(pending) = place else {
                    continue;
                };
                let value = self
                    .pending
                    .remove(&(page, pending.id()))
                    .expect("its bytes");
                let pointer = ValuePointer::new(FileId::MIN, self.next_offset, pending.value_len())
                    .expect("a pointer");
                self.next_offset += pointer.extent_len(V1);
                let step = ValueMove::in_flat_leaf(page, place, pointer).expect("a step");
                let mut image = Vec::new();
                step.apply_to_flat_leaf(&self.pages[&page], V1, self.config.capacity(), &mut image)
                    .expect("the entry holds the pending value");
                self.pages.insert(page, image);
                self.value_log.insert(pointer, (page, value));
                assert!(self.live.insert(pointer));
            }
        }
        self.base = self.pages.clone();
        self.revision += 1;
    }

    /// A commit: the run of each changed page takes the page from what the
    /// last commit left to what it is now, and the next write starts with
    /// no page taken. Returns the pages that have a run and the bytes of
    /// all runs.
    fn commit(&mut self) -> (Vec<PageId>, usize) {
        let (mut changed, mut logged) = (Vec::new(), 0);
        let mut commit = Commit::default();
        for (&page, image) in &self.pages {
            let base = self.base.get(&page).map(Vec::as_slice);
            if !is_changed(base, image).expect("pages") {
                continue;
            }
            assert!(
                self.taken.contains(&page),
                "page {page} changed and was not taken"
            );
            let mut bytes = Vec::new();
            let mut run = FlatRunWriter::new(page, &mut bytes);
            let records = push_changes(base, image, &mut run).expect("records");
            run.finish().expect("a run");
            let run = FlatRun::decode(&bytes, V1).expect("a run");
            assert_eq!(run.changes().len(), records);
            let mut applied = Vec::new();
            run.apply(base, V1, self.config.capacity(), &mut applied)
                .expect("the run applies to the page as it was");
            assert_eq!(&applied, image, "the run of page {page} gives another page");
            assert!(bytes.len() <= 7 + image.len(), "a run longer than an image");
            changed.push(page);
            logged += bytes.len();
            commit.runs.push((page, bytes));
        }
        commit.freed = self
            .base
            .keys()
            .filter(|page| !self.pages.contains_key(page))
            .copied()
            .collect();
        self.log.push(commit);
        self.base = self.pages.clone();
        self.taken.clear();
        (changed, logged)
    }

    /// The pages that the first `commits` commits leave, made from their
    /// runs alone, as a recovery makes them.
    fn replay(&self, commits: usize) -> Self {
        let mut pages = Self::new(self.config);
        for commit in &self.log[..commits] {
            for (page, bytes) in &commit.runs {
                let run = FlatRun::decode(bytes, V1).expect("a run");
                let base = pages.pages.get(page).map(Vec::as_slice);
                let mut image = Vec::new();
                run.apply(base, V1, self.config.capacity(), &mut image)
                    .expect("the run applies");
                pages.pages.insert(*page, image);
            }
            for page in &commit.freed {
                assert!(pages.pages.remove(page).is_some());
            }
        }
        pages
    }

    /// The changes of the run that the last commit wrote for `page`.
    fn last_run(&self, page: PageId) -> Vec<String> {
        let commit = self.log.last().expect("a commit");
        let run = commit.runs.iter().find(|(of, _)| *of == page);
        let (_, bytes) = run.expect("a run of the page");
        FlatRun::decode(bytes, V1)
            .expect("a run")
            .changes()
            .map(|change| {
                let name = format!("{change:?}");
                name.split([' ', '(']).next().unwrap_or_default().to_owned()
            })
            .collect()
    }

    fn name(&mut self) -> [u8; 8] {
        self.next_value += 1;
        (1 << 63 | self.next_value).to_le_bytes()
    }
}

impl FlatPages for Mem {
    type Page<'a> = &'a [u8];

    fn page(&self, page: PageId) -> Result<&[u8], FlatError> {
        let image = self.pages.get(&page).ok_or(FlatError::Freed { page })?;
        Ok(image)
    }

    fn read_value(
        &self,
        held: &&[u8],
        page: PageId,
        slot: usize,
        place: ValuePlace,
        out: &mut Vec<u8>,
    ) -> Result<(), FlatError> {
        assert!(
            Self::places_of(held).contains(&(slot, place)),
            "entry {slot} of the page that is held does not hold {place:?}"
        );
        match place {
            ValuePlace::Pending(pending) => {
                let value = self.pending.get(&(page, pending.id()));
                out.extend_from_slice(value.ok_or(FlatError::NoValue { page })?);
            }
            ValuePlace::Placed(pointer) => {
                let (owner, value) = &self.value_log[&pointer];
                assert_eq!(*owner, page, "a value read for a page that does not own it");
                out.extend_from_slice(value);
            }
        }
        Ok(())
    }

    fn revision(&self) -> Revision {
        Revision::Written(self.revision)
    }
}

impl FlatPagesMut for Mem {
    fn allocate(&mut self) -> Result<PageId, FlatError> {
        self.next_page += 1;
        let page = page_id(self.next_page);
        self.taken.insert(page);
        Ok(page)
    }

    fn take(&mut self, page: PageId) -> Result<Vec<u8>, FlatError> {
        if self
            .step_before_take
            .as_ref()
            .is_some_and(|(of, _)| *of == page)
        {
            let (_, image) = self.step_before_take.take().expect("a step");
            self.pages.insert(page, image.clone());
            self.base.insert(page, image);
        }
        self.taken.insert(page);
        self.takes += 1;
        Ok(self.page(page)?.to_vec())
    }

    fn write(&mut self, page: PageId, image: &[u8]) -> Result<(), FlatError> {
        assert!(
            self.taken.contains(&page),
            "page {page} written and not taken"
        );
        let read = FlatPage::decode(image, V1).expect("a well-formed page");
        assert_eq!((read.id(), read.as_bytes().len()), (page, image.len()));
        assert!(image.len() <= self.config.capacity());
        self.leaf_writes += u64::from(read.leaf().is_some());
        if read.leaf().is_some() {
            self.leaf_pages.push(page);
        }
        self.pages.insert(page, image.to_vec());
        for (_, place) in self.places(page) {
            if let ValuePlace::Pending(pending) = place {
                assert!(self.pending.contains_key(&(page, pending.id())));
            }
        }
        self.revision += 1;
        Ok(())
    }

    fn free(&mut self, page: PageId) -> Result<(), FlatError> {
        assert!(
            self.taken.contains(&page),
            "page {page} freed and not taken"
        );
        assert!(
            self.pending.keys().all(|(owner, _)| *owner != page),
            "page {page} freed with a value"
        );
        assert!(self.pages.remove(&page).is_some());
        self.freed.push(page);
        self.revision += 1;
        Ok(())
    }

    fn hold_value(&mut self, page: PageId, value: &[u8]) -> Result<PendingValue, FlatError> {
        assert!(
            self.taken.contains(&page),
            "a value for page {page}, not taken"
        );
        let name = self.name();
        self.pending.insert((page, name), value.to_vec());
        let held = PendingValue::new(name, value.len() as u32)?;
        self.held.push((page, held));
        Ok(held)
    }

    fn drop_value(&mut self, page: PageId, place: ValuePlace) -> Result<(), FlatError> {
        assert!(
            self.taken.contains(&page),
            "a value of page {page}, not taken"
        );
        match place {
            ValuePlace::Pending(pending) => {
                assert!(self.pending.remove(&(page, pending.id())).is_some());
            }
            ValuePlace::Placed(pointer) => {
                assert_eq!(self.value_log[&pointer].0, page);
                assert!(self.live.remove(&pointer), "{pointer:?} released twice");
                self.released.push(pointer);
            }
        }
        Ok(())
    }

    fn carry_value(
        &mut self,
        from: PageId,
        slot: usize,
        to: PageId,
        place: ValuePlace,
    ) -> Result<PendingValue, FlatError> {
        assert_ne!(from, to);
        let mut value = Vec::new();
        let held = self.page(from)?;
        self.read_value(&held, from, slot, place, &mut value)?;
        let held = self.hold_value(to, &value)?;
        self.held.pop();
        self.drop_value(from, place)?;
        self.carried.push(Carry {
            from,
            to,
            place,
            held,
        });
        Ok(held)
    }
}

/// Reads every record of the keyspace from its pages, without the tree's
/// code, and checks what must hold of the pages on the way.
fn records(pages: &Mem, tree: &FlatTree) -> Vec<(Vec<u8>, Vec<u8>)> {
    struct Walk<'a> {
        pages: &'a Mem,
        config: FlatConfig,
        seen: BTreeSet<PageId>,
        places: HashSet<ValuePlace>,
        /// What the tree that is walked holds, in order.
        found: Vec<Found>,
    }

    /// An entry of a tree: a record, or the bucket of the long keys with
    /// one head, which is the tree with this root.
    enum Found {
        Record(Vec<u8>, Vec<u8>),
        Bucket(Vec<u8>, PageId),
    }

    impl Walk<'_> {
        fn visit(
            &mut self,
            root: PageId,
            page: PageId,
            above: Option<u8>,
            bounds: (Option<&[u8]>, Option<&[u8]>),
        ) {
            let config = self.config;
            let (lower, upper) = bounds;
            assert!(self.seen.insert(page), "page {page} is reached twice");
            let read = self.pages.image(page);
            assert_eq!(read.id(), page);
            assert!(read.as_bytes().len() <= config.capacity());
            if let Some(above) = above {
                assert_eq!(read.level() + 1, above, "page {page} is at the wrong level");
            }
            let key = |slot: usize| match read {
                FlatPage::Leaf(leaf) => leaf.key(slot),
                FlatPage::Branch(branch) => branch.separator(slot),
            };
            let ends = (key(0), read.len().checked_sub(1).and_then(key));
            let shared = match ends {
                (Some(first), Some(last)) => common_prefix_len(&first.to_vec(), &last.to_vec()),
                _ => 0,
            };
            assert_eq!(read.prefix().len(), shared, "the prefix of page {page}");
            let inside = |key: &[u8]| {
                lower.is_none_or(|lower| key >= lower) && upper.is_none_or(|upper| key < upper)
            };
            match read {
                FlatPage::Leaf(leaf) => {
                    assert!(page == root || !leaf.is_empty(), "leaf {page} is empty");
                    for (slot, (key, value)) in leaf.iter().enumerate() {
                        let key = key.to_vec();
                        assert!(inside(&key), "a key of page {page} is outside its bounds");
                        assert!(key.len() <= config.key_limit() + 1);
                        if config.is_bucket(key.len()) {
                            // A bucket: the keys that begin with its head
                            // are in a tree of their own, which is not
                            // empty.
                            assert_eq!(key.last(), Some(&0));
                            let FlatValue::Inline(inner) = value else {
                                panic!("a bucket with a value out of line");
                            };
                            let inner = PageId::decode(inner.try_into().expect("a page id"));
                            let head = key[..config.key_limit()].to_vec();
                            self.found.push(Found::Bucket(head, inner.expect("a page")));
                            continue;
                        }
                        let mut bytes = Vec::new();
                        match value {
                            FlatValue::Inline(value) => {
                                assert!(value.len() <= config.max_inline());
                                bytes.extend_from_slice(value);
                            }
                            FlatValue::OutOfLine(place) => {
                                let place = place_of(place);
                                assert!(place.value_len() as usize > config.max_inline());
                                assert!(self.places.insert(place), "two entries hold {place:?}");
                                let held = self.pages.page(page).expect("the page");
                                self.pages
                                    .read_value(&held, page, slot, place, &mut bytes)
                                    .expect("the value");
                                assert_eq!(bytes.len(), place.value_len() as usize);
                            }
                        }
                        self.found.push(Found::Record(key, bytes));
                    }
                }
                FlatPage::Branch(branch) => {
                    let separators: Vec<Vec<u8>> = (0..branch.len())
                        .map(|slot| branch.separator(slot).expect("a separator").to_vec())
                        .collect();
                    for separator in &separators {
                        assert!(inside(separator) && Some(separator.as_slice()) != lower);
                    }
                    for index in 0..=branch.len() {
                        let below = index.checked_sub(1).map(|slot| separators[slot].as_slice());
                        let over = separators.get(index).map(Vec::as_slice);
                        let child = branch.child(index).expect("a child");
                        let bounds = (below.or(lower), over.or(upper));
                        self.visit(root, child, Some(branch.level()), bounds);
                    }
                }
            }
        }
    }

    let mut walk = Walk {
        pages,
        config: tree.config(),
        seen: BTreeSet::new(),
        places: HashSet::new(),
        found: Vec::new(),
    };
    // A long key is as many trees deep as it has heads, so the trees are
    // walked one after the other and not one inside the other.
    walk.visit(tree.root(), tree.root(), None, (None, None));
    let mut out = Vec::new();
    let mut heads = Vec::new();
    let mut inside = vec![(0, std::mem::take(&mut walk.found).into_iter())];
    while let Some((_, found)) = inside.last_mut() {
        match found.next() {
            Some(Found::Record(key, value)) => out.push(([heads.as_slice(), &key].concat(), value)),
            Some(Found::Bucket(head, root)) => {
                walk.visit(root, root, None, (None, None));
                let found = std::mem::take(&mut walk.found);
                assert!(!found.is_empty(), "an empty bucket");
                inside.push((heads.len(), found.into_iter()));
                heads.extend_from_slice(&head);
            }
            None => {
                let (len, _) = inside.pop().expect("a tree");
                heads.truncate(len);
            }
        }
    }
    let all: BTreeSet<PageId> = pages.pages.keys().copied().collect();
    assert_eq!(walk.seen, all, "a page is lost, or a freed one is reached");
    let pointers: HashSet<ValuePointer> =
        walk.places.iter().filter_map(ValuePlace::pointer).collect();
    assert_eq!(
        pointers, pages.live,
        "the pointers held are not those counted"
    );
    let pending = walk
        .places
        .iter()
        .filter(|place| place.is_pending())
        .count();
    assert_eq!(pending, pages.pending.len(), "a pending value has no entry");
    assert!(out.is_sorted_by(|a, b| a.0 < b.0), "keys out of order");
    out
}

fn get(pages: &impl FlatPages, tree: &FlatTree, key: &[u8]) -> Option<Vec<u8>> {
    let mut value = Vec::new();
    tree.get(pages, key, &mut value)
        .expect("a read")
        .then_some(value)
}

/// `len` bytes that depend on `seed`.
fn bytes(seed: u64, len: usize) -> Vec<u8> {
    let mut rng = SimRng::new(seed);
    (0..len).map(|_| rng.next_u64() as u8).collect()
}
