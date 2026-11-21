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
/// `page`, to `out`. `held` is the page the entry was read from.
pub(super) fn read_value<P: FlatPages + ?Sized>(
    pages: &P,
    held: &P::Page<'_>,
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
        FlatValue::OutOfLine(bytes) => pages.read_value(held, page, slot, place(bytes)?, out),
    }
}

/// Drops the value out of line that `value`, of an entry of the leaf
/// `page`, holds.
pub(super) fn drop_held<P: FlatPagesMut + ?Sized>(
    pages: &mut P,
    page: PageId,
    value: ValueRef<'_>,
) -> Result<(), FlatError> {
    match value {
        ValueRef::Held { place: held, .. } => pages.drop_value(page, place(held)?),
        ValueRef::Inline(_) | ValueRef::New(_) => Ok(()),
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

    /// The key in two parts: what its page stores once, and the rest.
    fn parts(&self) -> (&[u8], &[u8]) {
        match self {
            Self::Page(key) => (key.prefix(), key.suffix()),
            Self::Whole(key) => (key, &[]),
        }
    }

    /// The number of leading bytes the key shares with `other`.
    fn shared(&self, other: &Self) -> usize {
        let ((head, tail), (other_head, other_tail)) = (self.parts(), other.parts());
        // Two keys of one page begin with the same bytes of its image.
        if std::ptr::eq(head, other_head) {
            return head.len() + common_prefix_len(tail, other_tail);
        }
        let same = |(a, b): &(&u8, &u8)| a == b;
        let bytes = head.iter().chain(tail);
        bytes
            .zip(other_head.iter().chain(other_tail))
            .take_while(same)
            .count()
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

/// What an entry takes of a page.
pub(super) trait Entry {
    /// The bytes of the page's header.
    const HEADER_LEN: usize;

    fn key(&self) -> KeyRef<'_>;

    /// The bytes the entry takes beside its key.
    fn rest_len(&self) -> usize;
}

impl Entry for LeafEntry<'_> {
    const HEADER_LEN: usize = FlatLeaf::HEADER_LEN;

    fn key(&self) -> KeyRef<'_> {
        self.key
    }

    fn rest_len(&self) -> usize {
        FlatLeaf::ENTRY_OVERHEAD + self.value.stored_len()
    }
}

impl Entry for BranchEntry<'_> {
    const HEADER_LEN: usize = FlatBranch::HEADER_LEN;

    fn key(&self) -> KeyRef<'_> {
        self.key
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

/// The prefix of a page that holds `entries`: what its keys share, which
/// is what the first and the last of them share. The prefix of a page is
/// always this one, so a page's bytes follow from what it holds.
pub(super) fn prefix<E: Entry>(entries: &[E]) -> Vec<u8> {
    let (Some(first), Some(last)) = (entries.first(), entries.last()) else {
        return Vec::new();
    };
    let mut prefix = first.key().to_vec();
    prefix.truncate(first.key().shared(&last.key()));
    prefix
}

/// The lengths of the pages that runs of some entries would be, each under
/// the prefix its own keys share.
pub(super) struct Layout {
    header: usize,
    /// For each entry, the bytes it takes with its whole key, summed up to
    /// and including it.
    sums: Vec<usize>,
    keys: Vec<usize>,
    /// For each entry but the last, what its key shares with the next.
    shared: Vec<usize>,
}

impl Layout {
    pub(super) fn of<E: Entry>(entries: &[E]) -> Self {
        let mut sum = 0;
        Self {
            header: E::HEADER_LEN,
            sums: entries
                .iter()
                .map(|entry| {
                    sum += entry.key().len() + entry.rest_len();
                    sum
                })
                .collect(),
            keys: entries.iter().map(|entry| entry.key().len()).collect(),
            shared: entries
                .windows(2)
                .map(|pair| pair[0].key().shared(&pair[1].key()))
                .collect(),
        }
    }

    fn entries(&self) -> usize {
        self.keys.len()
    }

    /// The length of the page that holds the entries from `start` up to
    /// `end`, whose keys share `prefix` bytes.
    fn len_under(&self, start: usize, end: usize, prefix: usize) -> usize {
        let before = start.checked_sub(1).map_or(0, |last| self.sums[last]);
        let whole = end.checked_sub(1).map_or(0, |last| self.sums[last]) - before;
        self.header + prefix + whole - (end - start) * prefix
    }

    /// The length of the page that holds the entries from `start` up to
    /// `end`, which is not before `start`.
    fn run_len(&self, start: usize, end: usize) -> usize {
        let prefix = if start == end {
            0
        } else {
            let shared = self.shared[start..end - 1].iter().copied().min();
            shared.unwrap_or(self.keys[start])
        };
        self.len_under(start, end, prefix)
    }

    /// The length of the page that holds every entry.
    pub(super) fn len(&self) -> usize {
        self.run_len(0, self.entries())
    }

    /// The end of the longest run from `start` that fits a page of
    /// `capacity`. It is not empty: an entry alone fits.
    fn fit(&self, start: usize, capacity: usize) -> usize {
        let mut prefix = self.keys[start];
        let mut end = start + 1;
        while end < self.entries() {
            let next = prefix.min(self.shared[end - 1]);
            if self.len_under(start, end + 1, next) > capacity {
                break;
            }
            prefix = next;
            end += 1;
        }
        end
    }

    /// Cuts the entries, which are too long for one page of `capacity`,
    /// into runs that each fit one, and returns where each run but the
    /// last ends.
    ///
    /// With `skip` 0 every entry is in a run, as the entries of a leaf are.
    /// With `skip` 1 the entry at each cut is in no run, as the separator
    /// that a branch gives to the page above it. No run is empty, because
    /// any two entries fit one page.
    ///
    /// Where one cut is enough, it is put where the two runs are nearest
    /// in length, unless the entry in position `added` is a new one that
    /// goes on a run of keys ([`Self::run_end`]). With more cuts each run
    /// is as long as it can be.
    pub(super) fn cuts(&self, capacity: usize, skip: usize, added: Option<usize>) -> Vec<usize> {
        let count = self.entries();
        let mut cuts = Vec::new();
        let mut start = 0;
        while start < count {
            let mut end = self.fit(start, capacity);
            if end == count {
                break;
            }
            // The entry at a cut leaves the page, and a run follows it.
            if skip == 1 && end + 1 == count && end > start + 1 {
                end -= 1;
            }
            cuts.push(end);
            start = end + skip;
        }
        if let [cut] = cuts.as_mut_slice() {
            let chosen = added.and_then(|added| self.run_end(added, capacity));
            *cut = chosen
                .or_else(|| self.middle(capacity, skip))
                .unwrap_or(*cut);
        }
        cuts
    }

    /// A cut for keys that arrive in order: where the new entry in
    /// position `added` ends a run of keys, the cut that keeps the run
    /// together, so that the keys which follow it find room at the end of
    /// their page and pages are left full.
    ///
    /// The new entry ends a run when at least two keys stand before it in
    /// the page, and the key after it shares less with it than all of them
    /// do, or there is no key after it. The cut is behind the new entry,
    /// or before it if the run is a page already.
    fn run_end(&self, added: usize, capacity: usize) -> Option<usize> {
        let run = self.shared.get(..added)?.iter().copied().min()?;
        if added < 2 || self.shared.get(added).is_some_and(|&next| next >= run) {
            return None;
        }
        let count = self.entries();
        let fits = |start, end| self.run_len(start, end) <= capacity;
        [added + 1, added]
            .into_iter()
            .find(|&cut| cut < count && fits(0, cut) && fits(cut, count))
    }

    /// The one cut that leaves two runs nearest in length.
    fn middle(&self, capacity: usize, skip: usize) -> Option<usize> {
        let count = self.entries();
        // What the keys before each position share, and those from it on.
        let mut before = vec![0; count + 1];
        let mut from = vec![0; count + 1];
        for end in 1..=count {
            before[end] = match end {
                1 => self.keys[0],
                _ => before[end - 1].min(self.shared[end - 2]),
            };
        }
        for start in (0..count).rev() {
            from[start] = match count - start {
                1 => self.keys[start],
                _ => from[start + 1].min(self.shared[start]),
            };
        }
        (1..count - skip)
            .map(|cut| {
                let start = cut + skip;
                let first = self.len_under(0, cut, before[cut]);
                let second = self.len_under(start, count, from[start]);
                (cut, first, second)
            })
            .filter(|&(_, first, second)| first <= capacity && second <= capacity)
            .min_by_key(|&(_, first, second)| first.abs_diff(second))
            .map(|(cut, ..)| cut)
    }
}

/// Writes the leaf `page` with `entries`.
///
/// A value that an entry of another page held is carried to this one, and a
/// new value is held for it.
pub(super) fn write_leaf<P: FlatPagesMut + ?Sized>(
    pages: &mut P,
    capacity: usize,
    page: PageId,
    entries: &[LeafEntry<'_>],
) -> Result<(), FlatError> {
    let mut image = FlatLeafBuilder::new(page, &prefix(entries), capacity)?;
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

/// Writes the branch `page` of `level` with the first child `first` and
/// `entries`.
pub(super) fn write_branch<P: FlatPagesMut + ?Sized>(
    pages: &mut P,
    capacity: usize,
    page: PageId,
    level: u8,
    first: PageId,
    entries: &[BranchEntry<'_>],
) -> Result<(), FlatError> {
    let mut image = FlatBranchBuilder::new(page, &prefix(entries), capacity, level, first)?;
    for entry in entries {
        match entry.key {
            KeyRef::Page(key) => image.push_key(key, entry.child)?,
            KeyRef::Whole(key) => image.push(key, entry.child)?,
        }
    }
    pages.write(page, &image.finish())
}
