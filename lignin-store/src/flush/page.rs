//! What write-back reads from a page in a frame, and how it builds a page
//! from the blocks of its chain. It knows the two page formats by their
//! codecs and no more: no key, no hash, no tree.

use lignin_format::{
    delta::{DeltaError, DeltaRun},
    flat_page::{FlatPage, FlatPageKind, FlatValue},
    segment::BlockContent,
    tree_page::{CellValue, TreePage, TreePageMut},
    value_log::{PendingValue, ValuePlace, ValuePointer},
    wal::ValueMove,
    FormatError, PageId, PageSize, SlotId,
};

use super::{config::max_run_len, error::FlushError};
use crate::{
    load::FRAME_FORMAT,
    segment::{BlockLocation, SegmentStore},
    table::{Link, LinkKind},
};

/// Which of the two page formats a frame holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PageKind {
    /// A tree page: it is written as a base image or as a run of deltas.
    Tree,
    /// A flat page: every block of it is its image (`page-delta.md` PD13).
    Flat,
}

/// The kind of the page in `frame`, by its first byte, as a loader tells
/// them apart.
pub(crate) fn kind_of(frame: &[u8]) -> PageKind {
    match frame.first() {
        Some(&code) if FlatPageKind::from_code(code, FRAME_FORMAT).is_ok() => PageKind::Flat,
        _ => PageKind::Tree,
    }
}

/// A cell of a tree page, or an entry of a flat leaf, whose value is not in
/// the value log yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PendingCell {
    /// The slot of the cell in a tree page. An entry of a flat leaf is
    /// found by the place it holds.
    pub(crate) slot: Option<SlotId>,
    pub(crate) pending: PendingValue,
}

/// The pending values of the page in `frame`.
pub(crate) fn pending_cells(frame: &[u8]) -> Result<Vec<PendingCell>, FormatError> {
    match kind_of(frame) {
        PageKind::Tree => Ok(TreePage::from_validated(frame, FRAME_FORMAT)?
            .pending_values()
            .map(|(slot, pending)| PendingCell {
                slot: Some(slot),
                pending,
            })
            .collect()),
        PageKind::Flat => {
            let Some(leaf) = FlatPage::from_validated(frame, FRAME_FORMAT)?.leaf() else {
                return Ok(Vec::new());
            };
            leaf.iter()
                .filter_map(|(_, value)| match value {
                    FlatValue::OutOfLine(place) => place.try_into().ok(),
                    FlatValue::Inline(_) => None,
                })
                .map(ValuePlace::decode)
                .filter_map(|place| match place {
                    Ok(place) => place.pending().map(|pending| {
                        Ok(PendingCell {
                            slot: None,
                            pending,
                        })
                    }),
                    Err(error) => Some(Err(error)),
                })
                .collect()
        }
    }
}

/// Appends the image of the page in `frame` to `out`, and returns whether
/// the page has a pending value: an image with one is not written to a
/// segment (`page-delta.md` PD16).
pub(crate) fn write_image(frame: &[u8], out: &mut Vec<u8>) -> Result<bool, FormatError> {
    match kind_of(frame) {
        PageKind::Tree => {
            let page = TreePage::from_validated(frame, FRAME_FORMAT)?;
            page.write_image(out);
            Ok(page.has_pending_value())
        }
        PageKind::Flat => {
            let page = FlatPage::from_validated(frame, FRAME_FORMAT)?;
            out.extend_from_slice(page.as_bytes());
            Ok(page.has_pending_value())
        }
    }
}

/// The page step that gives the pending value of `cell`, in the page
/// `page`, the pointer `to`.
pub(super) fn placing(
    page: PageId,
    cell: &PendingCell,
    to: ValuePointer,
) -> Result<ValueMove, FormatError> {
    let from = ValuePlace::Pending(cell.pending);
    match cell.slot {
        Some(slot) => ValueMove::in_tree(page, slot, from, to),
        None => ValueMove::in_flat_leaf(page, from, to),
    }
}

/// Whether the page in `frame` still holds what `step` says it held.
pub(crate) fn is_due(frame: &[u8], step: &ValueMove) -> Result<bool, FormatError> {
    match kind_of(frame) {
        PageKind::Tree => Ok(step.is_due_in_tree(&TreePage::from_validated(frame, FRAME_FORMAT)?)),
        PageKind::Flat => step.is_due_in_flat_leaf(frame, FRAME_FORMAT),
    }
}

/// Makes the change of `step` to the page in `frame`, a copy of the page
/// the step is due in.
pub(crate) fn apply_step(
    frame: &mut [u8],
    size: PageSize,
    step: &ValueMove,
) -> Result<(), FormatError> {
    match step.slot() {
        Some(slot) => TreePageMut::open(frame, size, FRAME_FORMAT)?
            .set_value(slot, CellValue::OutOfLine(step.to())),
        None => {
            let mut image = Vec::new();
            step.apply_to_flat_leaf(frame, FRAME_FORMAT, size.image_capacity(), &mut image)?;
            let (start, rest) = frame.split_at_mut(image.len());
            start.copy_from_slice(&image);
            rest.fill(0);
            Ok(())
        }
    }
}

/// The links of a chain that give the page as the link at `through` leaves
/// it: the nearest base image at or before it, and the runs after that.
pub(super) fn readable(links: &[Link], through: usize) -> Option<&[Link]> {
    let upto = links.get(..=through)?;
    let base = upto
        .iter()
        .rposition(|link| !matches!(link.kind, LinkKind::Delta(_)))?;
    match upto[base].kind {
        LinkKind::Base(_) => Some(&upto[base..]),
        _ => None,
    }
}

/// Memory for [`materialize`], kept from one call to the next.
#[derive(Debug, Default)]
pub(super) struct ChainBuffers {
    block: Vec<u8>,
    run: Vec<u8>,
    locations: Vec<BlockLocation>,
    read: u64,
}

impl ChainBuffers {
    /// How many blocks the last [`materialize`] read from their segments,
    /// whether it then built the page or not.
    pub(super) fn read(&self) -> u64 {
        self.read
    }
}

/// Builds the page `page` in `frame` from the blocks of `links`: a base
/// image and the runs after it, as [`readable`] gives them. Every block is
/// read with its checksum verified and checked as a loader checks it.
///
/// The caller holds an operation guard, which keeps the blocks readable.
pub(super) fn materialize(
    segments: &SegmentStore,
    size: PageSize,
    page: PageId,
    links: &[Link],
    frame: &mut [u8],
    buffers: &mut ChainBuffers,
) -> Result<PageKind, FlushError> {
    let refused = FlushError::page(page);
    let wrong_page = |found| DeltaError::WrongPage {
        found,
        expected: page,
    };
    buffers.read = 0;
    // As a loader does: the file layer is told of every block of the
    // chain before the first is read.
    buffers.locations.clear();
    (buffers.locations).extend(links.iter().filter_map(Link::location));
    segments.will_read(&buffers.locations);
    let mut kind = None;
    for link in links {
        let Some(location) = link.location() else {
            return Err(refused(DeltaError::MissingBase.into()));
        };
        let version = segments.format_version(location.file)?;
        match (kind, link.kind) {
            (None, LinkKind::Base(_)) => {
                let block = segments.read(location, size.image_capacity(), &mut buffers.block)?;
                buffers.read += 1;
                if block.content() != BlockContent::Base {
                    return Err(refused(DeltaError::MissingBase.into()));
                }
                let (image, rest) = frame
                    .split_at_mut_checked(block.payload_len())
                    .ok_or_else(|| FlushError::page(page)(DeltaError::MissingBase.into()))?;
                block
                    .copy_payload_to(image)
                    .map_err(FlushError::page(page))?;
                rest.fill(0);
                let found = match kind_of(image) {
                    PageKind::Flat => FlatPage::decode(image, version)
                        .map_err(FlushError::page(page))?
                        .id(),
                    PageKind::Tree => TreePage::decode(image, version, size)
                        .map_err(FlushError::page(page))?
                        .id(),
                };
                if found != page {
                    return Err(refused(wrong_page(found).into()));
                }
                kind = Some(kind_of(frame));
            }
            (Some(PageKind::Tree), LinkKind::Delta(_)) => {
                let block = segments.read(location, max_run_len(size), &mut buffers.block)?;
                buffers.read += 1;
                if block.content() != BlockContent::Delta {
                    return Err(refused(DeltaError::MissingBase.into()));
                }
                buffers.run.clear();
                buffers.run.resize(block.payload_len(), 0);
                block
                    .copy_payload_to(&mut buffers.run)
                    .map_err(FlushError::page(page))?;
                let run =
                    DeltaRun::decode(&buffers.run, version).map_err(FlushError::page(page))?;
                if run.page() != page || run.created().is_some() {
                    return Err(refused(wrong_page(run.page()).into()));
                }
                run.apply(frame, size, FRAME_FORMAT)
                    .map_err(FlushError::page(page))?;
            }
            _ => return Err(refused(DeltaError::MissingBase.into())),
        }
    }
    kind.ok_or_else(|| FlushError::page(page)(DeltaError::MissingBase.into()))
}
