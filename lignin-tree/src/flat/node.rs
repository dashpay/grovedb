//! The entries of one flat page, taken apart so that the page can be built
//! again: keys and values borrowed from the old image, sizes, and where a
//! page that has outgrown its image is cut.

use lignin_format::{
    flat_page::{
        common_prefix_len, FlatBranch, FlatBranchBuilder, FlatKey, FlatLeaf, FlatLeafBuilder,
        FlatPage, FlatValue,
    },
    value_log::ValuePlace,
    PageId,
};
use lignin_store::load::FRAME_FORMAT;

use super::{
    error::FlatError,
    pages::{FlatPages, FlatPagesMut},
};

/// The flat page at the start of `bytes`: a frame, or an image.
pub(super) fn view(bytes: &[u8]) -> Result<FlatPage<'_>, FlatError> {
    Ok(FlatPage::from_validated(bytes, FRAME_FORMAT)?)
}

/// The leaf at the start of `bytes`, which is the page `page`.
pub(super) fn leaf(bytes: &[u8], page: PageId) -> Result<FlatLeaf<'_>, FlatError> {
    view(bytes)?.leaf().ok_or(FlatError::Damaged {
        page,
        what: "a branch where a leaf is expected",
    })
}

/// The branch at the start of `bytes`, which is the page `page`.
pub(super) fn branch(bytes: &[u8], page: PageId) -> Result<FlatBranch<'_>, FlatError> {
    view(bytes)?.branch().ok_or(FlatError::Damaged {
        page,
        what: "a leaf where a branch is expected",
    })
}

/// The child in position `index` of `branch`.
pub(super) fn child(branch: &FlatBranch<'_>, index: usize) -> Result<PageId, FlatError> {
    branch.child(index).ok_or(FlatError::Damaged {
        page: branch.id(),
        what: "a branch without the child its slots name",
    })
}

/// The place that the 16 bytes of an out-of-line value are.
pub(super) fn place(bytes: &[u8]) -> Result<ValuePlace, FlatError> {
    let bytes = bytes
        .try_into()
        .map_err(|_| lignin_format::flat_page::FlatPageError::PointerLength {
            slot: 0,
            len: bytes.len(),
        })
        .map_err(lignin_format::FormatError::from)?;
    Ok(ValuePlace::decode(bytes)?)
}

/// Appends the value of `value`, an entry in position `slot` of the leaf
/// `page`, to `out`.
pub(super) fn read_value<P: FlatPages + ?Sized>(
    pages: &P,
    page: PageId,
    slot: usize,
    value: FlatValue<'_>,
    out: &mut Vec<u8>,
) -> Result<(), FlatError> {
    match value {
        FlatValue::Inline(bytes) => {
            out.extend_from_slice(bytes);
            Ok(())
        }
        FlatValue::OutOfLine(bytes) => pages.read_value(page, slot, place(bytes)?, out),
    }
}

/// A key of an entry: borrowed from the page it was read from, or from the
/// caller.
#[derive(Debug, Clone, Copy)]
pub(super) enum KeyRef<'a> {
    Page(FlatKey<'a>),
    Whole(&'a [u8]),
}

impl KeyRef<'_> {
    pub(super) fn len(&self) -> usize {
        match self {
            Self::Page(key) => key.len(),
            Self::Whole(key) => key.len(),
        }
    }

    pub(super) fn to_vec(self) -> Vec<u8> {
        match self {
            Self::Page(key) => key.to_vec(),
            Self::Whole(key) => key.to_vec(),
        }
    }

    pub(super) fn starts_with(&self, prefix: &[u8]) -> bool {
        match self {
            Self::Whole(key) => key.starts_with(prefix),
            Self::Page(key) => match prefix.split_at_checked(key.prefix().len()) {
                None => key.prefix().starts_with(prefix),
                Some((head, tail)) => head == key.prefix() && key.suffix().starts_with(tail),
            },
        }
    }
}

/// What an entry of a leaf holds for its key.
#[derive(Debug, Clone, Copy)]
pub(super) enum ValueRef<'a> {
    /// The value, in the page.
    Inline(&'a [u8]),
    /// The 16 bytes of the place of a value out of line, which the entry in
    /// position `slot` of the page `home` holds.
    Held {
        place: &'a [u8],
        home: PageId,
        slot: usize,
    },
    /// A value that goes out of line and has no place yet.
    New(&'a [u8]),
}

impl ValueRef<'_> {
    fn stored_len(&self) -> usize {
        match self {
            Self::Inline(value) => value.len(),
            Self::Held { .. } | Self::New(_) => ValuePlace::ENCODED_LEN,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(super) struct LeafEntry<'a> {
    pub(super) key: KeyRef<'a>,
    pub(super) value: ValueRef<'a>,
}

/// A separator of a branch, with the child to its right.
#[derive(Debug, Clone, Copy)]
pub(super) struct BranchEntry<'a> {
    pub(super) key: KeyRef<'a>,
    pub(super) child: PageId,
}

/// What an entry takes of a page: its key without the page's prefix, and
/// the rest of it.
pub(super) trait Sizes {
    fn key_len(&self) -> usize;
    fn rest_len(&self) -> usize;

    fn len_under(&self, prefix_len: usize) -> usize {
        self.key_len() - prefix_len + self.rest_len()
    }
}

impl Sizes for LeafEntry<'_> {
    fn key_len(&self) -> usize {
        self.key.len()
    }

    fn rest_len(&self) -> usize {
        FlatLeaf::ENTRY_OVERHEAD + self.value.stored_len()
    }
}

impl Sizes for BranchEntry<'_> {
    fn key_len(&self) -> usize {
        self.key.len()
    }

    fn rest_len(&self) -> usize {
        FlatBranch::ENTRY_OVERHEAD
    }
}

/// The entries of `leaf`, in key order.
pub(super) fn leaf_entries<'a>(leaf: &FlatLeaf<'a>) -> Vec<LeafEntry<'a>> {
    let home = leaf.id();
    leaf.iter()
        .enumerate()
        .map(|(slot, (key, value))| LeafEntry {
            key: KeyRef::Page(key),
            value: match value {
                FlatValue::Inline(value) => ValueRef::Inline(value),
                FlatValue::OutOfLine(place) => ValueRef::Held { place, home, slot },
            },
        })
        .collect()
}

/// The separators of `branch`, each with the child to its right.
pub(super) fn branch_entries<'a>(
    branch: &FlatBranch<'a>,
) -> Result<Vec<BranchEntry<'a>>, FlatError> {
    (0..branch.len())
        .map(|slot| {
            let key = branch.separator(slot).ok_or(FlatError::Damaged {
                page: branch.id(),
                what: "a branch without the separator its slots name",
            })?;
            Ok(BranchEntry {
                key: KeyRef::Page(key),
                child: child(branch, slot + 1)?,
            })
        })
        .collect()
}

/// Whether every key of `entries`, which are in key order, starts with
/// `prefix`: the first and the last do.
pub(super) fn all_start_with<'a>(
    prefix: &[u8],
    mut keys: impl DoubleEndedIterator<Item = KeyRef<'a>>,
) -> bool {
    let first = keys.next();
    let last = keys.next_back().or(first);
    first.is_none_or(|key| key.starts_with(prefix))
        && last.is_none_or(|key| key.starts_with(prefix))
}

/// The length of the image of a page with a header of `header_len` bytes,
/// the prefix `prefix_len` bytes long and `entries`.
pub(super) fn image_len<E: Sizes>(header_len: usize, prefix_len: usize, entries: &[E]) -> usize {
    header_len
        + prefix_len
        + entries
            .iter()
            .map(|entry| entry.len_under(prefix_len))
            .sum::<usize>()
}

/// What the two bounds of a page's key range share: the prefix a page is
/// given when it is built for a split, a merge or a key outside the prefix
/// it has. A range that is open on a side shares nothing.
pub(super) fn shared<'a>(lower: Option<&'a [u8]>, upper: Option<&[u8]>) -> &'a [u8] {
    match (lower, upper) {
        (Some(lower), Some(upper)) => &lower[..common_prefix_len(lower, upper)],
        _ => &[],
    }
}

/// Cuts entries of the lengths `lens`, which together are too long for one
/// page, into runs that each take at most `room` bytes, and returns where
/// each run but the last ends.
///
/// With `skip` 0 every entry is in a run, as the entries of a leaf are.
/// With `skip` 1 the entry at each cut is in no run, as the separator that
/// a branch gives to the page above it. No run is empty, because any two
/// entries fit one page.
///
/// `even` makes the runs about equally long. Without it each run is as
/// long as it can be, which leaves a page full when keys arrive in order.
pub(super) fn cuts(lens: &[usize], room: usize, skip: usize, even: bool) -> Vec<usize> {
    let full = fill(lens, room, skip, |_, _| usize::MAX);
    if !even {
        return full;
    }
    let runs = full.len() + 1;
    fill(lens, room, skip, |left, done| {
        left.div_ceil(runs.saturating_sub(done).max(1))
    })
}

/// The cuts of [`cuts`], with each run ended once it has reached
/// `target(bytes left, runs made)`.
fn fill(
    lens: &[usize],
    room: usize,
    skip: usize,
    target: impl Fn(usize, usize) -> usize,
) -> Vec<usize> {
    let mut cuts = Vec::new();
    let mut left: usize = lens.iter().sum();
    let mut at = 0;
    while let Some(&first) = lens.get(at) {
        let target = target(left, cuts.len());
        let (mut run, mut end) = (first, at + 1);
        while let Some(&next) = lens.get(end)
            && run + next <= room
            && run < target
        {
            run += next;
            end += 1;
        }
        let Some(&cut) = lens.get(end) else { break };
        if skip == 1 && end + 1 == lens.len() {
            // The entry at a cut leaves the page, and here no entry would
            // follow it. It joins this run, or the run's last entry is the
            // one that leaves.
            if run + cut <= room {
                break;
            }
            end -= 1;
            run -= lens[end];
        }
        cuts.push(end);
        left -= run + lens[end..end + skip].iter().sum::<usize>();
        at = end + skip;
    }
    cuts
}

/// Writes the leaf `page` with the prefix `prefix` and `entries`.
///
/// A value that an entry of another page held is carried to this one, and a
/// new value is held for it.
pub(super) fn write_leaf<P: FlatPagesMut + ?Sized>(
    pages: &mut P,
    capacity: usize,
    page: PageId,
    prefix: &[u8],
    entries: &[LeafEntry<'_>],
) -> Result<(), FlatError> {
    let mut image = FlatLeafBuilder::new(page, prefix, capacity)?;
    for entry in entries {
        let held;
        let value = match entry.value {
            ValueRef::Inline(value) => FlatValue::Inline(value),
            ValueRef::Held { place, home, .. } if home == page => FlatValue::OutOfLine(place),
            ValueRef::Held { place, home, slot } => {
                let carried = pages.carry_value(home, slot, page, self::place(place)?)?;
                held = ValuePlace::Pending(carried).encode();
                FlatValue::OutOfLine(&held)
            }
            ValueRef::New(value) => {
                held = ValuePlace::Pending(pages.hold_value(page, value)?).encode();
                FlatValue::OutOfLine(&held)
            }
        };
        match entry.key {
            KeyRef::Page(key) => image.push_key(key, value)?,
            KeyRef::Whole(key) => image.push(key, value)?,
        }
    }
    pages.write(page, &image.finish())
}

/// Writes the branch `page` of `level` with the prefix `prefix`, the first
/// child `first` and `entries`.
pub(super) fn write_branch<P: FlatPagesMut + ?Sized>(
    pages: &mut P,
    capacity: usize,
    page: PageId,
    level: u8,
    first: PageId,
    prefix: &[u8],
    entries: &[BranchEntry<'_>],
) -> Result<(), FlatError> {
    let mut image = FlatBranchBuilder::new(page, prefix, capacity, level, first)?;
    for entry in entries {
        match entry.key {
            KeyRef::Page(key) => image.push_key(key, entry.child)?,
            KeyRef::Whole(key) => image.push(key, entry.child)?,
        }
    }
    pages.write(page, &image.finish())
}
