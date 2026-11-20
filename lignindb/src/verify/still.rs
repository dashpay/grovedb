//! What a still store keeps beside its pages, compared with them: the page
//! ids, the counts of live values, and the page table with its blocks.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use lignin_format::{
    delta::{Delta, DeltaRun},
    flat_page::{FlatPage, FlatPageKind},
    segment::{BlockContent, FRAME_LEN, MAX_BODY_LEN},
    tree_page::{CellValue, TreePage, TreePageMut},
    FormatVersion, PageId,
};
use lignin_store::{
    alloc::PageState,
    flush::max_run_len,
    segment::BlockLocation,
    table::{Link, LinkKind},
};

use super::{
    pages::{Kind, Seen},
    Place, Rule, Run, SegmentUse,
};

/// A run that states more than this many times the length of its block is
/// refused before memory is taken for it: no LZ4 block reaches it.
const LZ4_MAX_EXPANSION: usize = 255;

/// The page that the links of a chain have built so far.
enum Built {
    /// No base image yet, or the page was freed.
    Nothing,
    /// A flat page: it takes no run.
    Flat,
    /// A tree page, in the frame.
    Tree(FormatVersion),
    /// A link was refused, and the links up to the next base image say
    /// nothing.
    Broken,
}

impl Run<'_, '_, '_> {
    /// Compares the states of the page ids with the pages of the version:
    /// the live ids are exactly the pages.
    pub(super) fn compare_ids(&mut self) {
        let Some(ids) = self.store.ids else {
            return;
        };
        let (mut from, mut states) = (Some(PageId::MIN), Vec::new());
        let mut live = BTreeSet::new();
        while let Some(page) = from {
            states.clear();
            from = ids.scan(page, &mut states);
            for (page, state) in &states {
                match state {
                    PageState::Live => {
                        live.insert(*page);
                        if self.see(*page) == Kind::None {
                            let what =
                                || "the id is live and no page of the version has it".to_owned();
                            self.add(Rule::IdWithoutPage, Place::Page(*page), what);
                        }
                    }
                    // An id that an open transaction holds was not read
                    // with the others: a page of the version under it is
                    // found here.
                    PageState::Private => {
                        self.see(*page);
                    }
                    PageState::Retired | PageState::Free => {}
                }
            }
        }
        let not_live: Vec<PageId> = (self.pages.keys())
            .filter(|page| !live.contains(page))
            .copied()
            .collect();
        for page in not_live {
            let state = ids.state(page);
            let what = || format!("the page is of the version and its id is {state}");
            self.add(Rule::IdNotLive, Place::Page(page), what);
        }
        self.census.ids = Some(ids.census());
    }

    /// Compares the store's counts of live values with the pointers that
    /// the pages hold.
    ///
    /// With a page that is not read there is nothing to compare: the page
    /// may hold pointers, and the store's count is of all of them.
    pub(super) fn compare_value_counts(&mut self) {
        let unread = (self.pages.values()).any(|seen| matches!(seen, Seen::Bad));
        if !self.store.values_counted || unread {
            return;
        }
        let counted: BTreeMap<_, _> = (self.store.values.counts().into_iter())
            .map(|count| (count.file, count))
            .collect();
        let files: BTreeSet<_> = counted.keys().chain(self.files.keys()).copied().collect();
        for file in files {
            let held = self.files.get(&file).copied().unwrap_or_default();
            let place = Place::ValueFile(file);
            match counted.get(&file) {
                Some(count) if count.miscounted => {
                    let what = || "the store found the file's count wrong".to_owned();
                    self.add(Rule::ValueCount, place, what);
                }
                Some(count) if (count.values, count.bytes) == held => {}
                Some(count) => {
                    let (values, bytes) = (count.values, count.bytes);
                    self.add(Rule::ValueCount, place, || {
                        format!(
                            "{values} values of {bytes} bytes are counted, and the pages hold \
                             {} pointers to {} bytes",
                            held.0, held.1
                        )
                    });
                }
                None => {
                    self.add(Rule::ValueCount, place, || {
                        format!(
                            "the pages hold {} pointers into a file with no count",
                            held.0
                        )
                    });
                }
            }
        }
    }

    /// Follows every link of the page table to its block: the rules of
    /// chains, the blocks, and what each page version in a segment holds.
    pub(super) fn check_chains(&mut self) {
        let (mut from, mut links) = (Some(PageId::MIN), Vec::new());
        let mut named = HashSet::new();
        let mut used: BTreeMap<_, (u64, u64)> = BTreeMap::new();
        let mut frame = vec![0; self.store.page_size.get()];
        let (mut block, mut payload) = (Vec::new(), Vec::new());
        while let Some(page) = from {
            links.clear();
            from = self.store.table.scan(page, &mut links);
            let mut chain = links.as_slice();
            while let Some(&(page, _)) = chain.first() {
                let len = chain.iter().take_while(|(of, _)| *of == page).count();
                let (of_page, rest) = chain.split_at(len);
                chain = rest;
                let mut built = Built::Nothing;
                let mut before: Option<Link> = None;
                for &(_, link) in of_page {
                    self.census.links += 1;
                    if let Some(location) = link.location() {
                        let of_file = used.entry(location.file).or_default();
                        of_file.0 += 1;
                        of_file.1 += u64::from(location.len);
                        if !named.insert(location) {
                            let what = || format!("a second link of page {page} names the block");
                            self.add(Rule::BlockNamedTwice, Place::Block(location), what);
                        }
                    }
                    if let Some(what) = chain_fault(before, link) {
                        self.add(Rule::Chain, Place::Page(page), || what.to_owned());
                    }
                    before = Some(link);
                    built = match link.kind {
                        LinkKind::Free => Built::Nothing,
                        LinkKind::Base(location) => {
                            self.read_image(page, location, &mut frame, &mut block)
                        }
                        LinkKind::Delta(location) => match built {
                            Built::Tree(version) => self.apply_run(
                                page,
                                location,
                                version,
                                &mut frame,
                                &mut block,
                                &mut payload,
                            ),
                            Built::Flat => {
                                let what = || format!("a run of deltas for the flat page {page}");
                                self.add(Rule::Block, Place::Block(location), what);
                                Built::Broken
                            }
                            // The link with nothing beneath it was reported.
                            Built::Nothing | Built::Broken => Built::Broken,
                        },
                    };
                }
            }
        }
        self.census.segments = (used.into_iter())
            .map(|(file, (blocks, bytes))| SegmentUse {
                file,
                blocks,
                bytes,
            })
            .collect();
    }

    /// Reads the block at `location` as `content`, and returns the length
    /// of its payload, which is in `payload`, with the format version of
    /// its segment. `fits` is asked for the length that the payload
    /// states before the payload is unpacked, and says what is wrong with
    /// a length that the store does not allow.
    fn read_block(
        &mut self,
        location: BlockLocation,
        content: BlockContent,
        max_payload_len: usize,
        block: &mut Vec<u8>,
        payload: &mut [u8],
        fits: impl FnOnce(usize) -> Result<(), String>,
    ) -> Option<(usize, FormatVersion)> {
        let place = Place::Block(location);
        let segments = self.store.segments;
        let version = match segments.format_version(location.file) {
            Ok(version) => version,
            Err(error) => {
                self.add(Rule::Block, place, || error.to_string());
                return None;
            }
        };
        let stored = match segments.read(location, max_payload_len, block) {
            Ok(stored) => stored,
            Err(error) => {
                self.add(Rule::Block, place, || error.to_string());
                return None;
            }
        };
        if stored.content() != content {
            let found = stored.content();
            let what = || format!("the link says {content:?} and the block holds {found:?}");
            self.add(Rule::Block, place, what);
            return None;
        }
        let len = stored.payload_len();
        let unpacked = fits(len).and_then(|()| match payload.get_mut(..len) {
            Some(into) => stored
                .copy_payload_to(into)
                .map_err(|error| error.to_string()),
            None => Err(format!("a payload of {len} bytes")),
        });
        if let Err(error) = unpacked {
            self.add(Rule::Block, place, || error);
            return None;
        }
        Some((len, version))
    }

    /// Puts the base image in the block at `location` into `frame`.
    fn read_image(
        &mut self,
        page: PageId,
        location: BlockLocation,
        frame: &mut [u8],
        block: &mut Vec<u8>,
    ) -> Built {
        let place = Place::Block(location);
        let size = self.store.page_size;
        let Some((len, version)) = self.read_block(
            location,
            BlockContent::Base,
            size.image_capacity(),
            block,
            frame,
            |_| Ok(()),
        ) else {
            return Built::Broken;
        };
        let (image, rest) = frame.split_at_mut(len);
        rest.fill(0);
        let is_flat =
            (image.first()).is_some_and(|&code| FlatPageKind::from_code(code, version).is_ok());
        let read = if is_flat {
            FlatPage::decode(image, version)
                .map(|flat| (flat.as_bytes().len(), flat.id(), flat.has_pending_value()))
        } else {
            TreePage::decode(image, version, size)
                .map(|tree| (tree.encoded_len(), tree.id(), tree.has_pending_value()))
        };
        let (image_len, found, pending) = match read {
            Ok(read) => read,
            Err(error) => {
                self.add(Rule::Block, place, || error.to_string());
                return Built::Broken;
            }
        };
        if image_len != len {
            let what = || format!("an image of {image_len} bytes in a payload of {len}");
            self.add(Rule::Block, place, what);
            return Built::Broken;
        }
        if found != page {
            let what = || format!("the image of page {found} in the chain of page {page}");
            self.add(Rule::Block, place, what);
            return Built::Broken;
        }
        if pending {
            let what = || format!("the base image of page {page} holds a pending value");
            self.add(Rule::PendingInSegment, place, what);
        }
        match is_flat {
            true => Built::Flat,
            false => Built::Tree(version),
        }
    }

    /// Applies the run in the block at `location` to the tree page in
    /// `frame`, record by record.
    fn apply_run(
        &mut self,
        page: PageId,
        location: BlockLocation,
        version: FormatVersion,
        frame: &mut [u8],
        block: &mut Vec<u8>,
        payload: &mut Vec<u8>,
    ) -> Built {
        let place = Place::Block(location);
        let size = self.store.page_size;
        let refused = |run: &mut Self, error: String| {
            run.add(Rule::Block, place, || error);
            Built::Broken
        };
        // A run is at most one page size, and the loader refuses a longer
        // one (`lignin_store::load`, `LoadError::RunTooLong`): a block that
        // is longer than such a run can be stored as before it is read, and
        // a run that states a longer payload before it is unpacked.
        let max = max_run_len(size);
        let too_long = |len: usize| {
            format!(
                "a run of {len} bytes for page {page}, and a run of this store has at most {max}"
            )
        };
        let stored_len = location.len as usize;
        if stored_len > FRAME_LEN + max {
            return refused(self, too_long(stored_len - FRAME_LEN));
        }
        // What is unpacked takes the place of every byte that is read.
        payload.resize(max, 0);
        let fits = |len: usize| {
            if len > stored_len.saturating_mul(LZ4_MAX_EXPANSION) {
                Err(format!(
                    "a payload of {len} bytes in a block of {stored_len}"
                ))
            } else if len > max {
                Err(too_long(len))
            } else {
                Ok(())
            }
        };
        let Some((len, run_version)) = self.read_block(
            location,
            BlockContent::Delta,
            MAX_BODY_LEN,
            block,
            payload,
            fits,
        ) else {
            return Built::Broken;
        };
        let run = match DeltaRun::decode(&payload[..len], run_version) {
            Ok(run) => run,
            Err(error) => return refused(self, error.to_string()),
        };
        if run.page() != page {
            let found = run.page();
            return refused(
                self,
                format!("a run of page {found} in the chain of page {page}"),
            );
        }
        if run.created().is_some() {
            return refused(self, format!("a run in a segment creates page {page}"));
        }
        let mut changed = match TreePageMut::open(frame, size, version) {
            Ok(changed) => changed,
            Err(error) => return refused(self, error.to_string()),
        };
        for delta in run.deltas() {
            if let Delta::Value {
                slot,
                value: CellValue::OutOfLine(pointer),
            } = delta
            {
                let pending = (changed.page().cell(slot)).and_then(|cell| cell.pending_value());
                if let Some(pending) = pending.filter(|p| p.value_len() != pointer.value_len()) {
                    self.add(Rule::PlacedLength, place, || {
                        format!(
                            "slot {slot} of page {page}: a pending value of {} bytes is placed \
                             under a pointer of {} bytes",
                            pending.value_len(),
                            pointer.value_len()
                        )
                    });
                }
            }
            if let Err(error) = delta.apply(&mut changed) {
                return refused(self, error.to_string());
            }
        }
        if changed.page().has_pending_value() {
            let what = || format!("page {page} holds a pending value at the end of the run");
            self.add(Rule::PendingInSegment, place, what);
        }
        Built::Tree(version)
    }
}

/// What is wrong with `link` behind `before` in a chain, if anything is
/// (`page-table.md`): the versions never go down, a delta follows a base or
/// a delta, and a free link is followed by a base.
pub(super) fn chain_fault(before: Option<Link>, link: Link) -> Option<&'static str> {
    let Some(before) = before else {
        return match link.kind {
            LinkKind::Delta(_) => Some("a chain begins with a run of deltas"),
            LinkKind::Base(_) | LinkKind::Free => None,
        };
    };
    if link.version < before.version {
        return Some("the versions of a chain go down");
    }
    match (before.kind, link.kind) {
        (LinkKind::Free, LinkKind::Delta(_)) => Some("a run of deltas follows a free link"),
        (LinkKind::Free, LinkKind::Free) => Some("a free link follows a free link"),
        _ => None,
    }
}
