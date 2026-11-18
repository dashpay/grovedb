//! What a commit writes to the log for a flat page: the records that take
//! the page from what it was to what it is (`log-format.md` LG19), and the
//! places its new values have in the log.

use std::cmp::Ordering;

use lignin_format::{
    flat_page::{FlatBranch, FlatKey, FlatLeaf, FlatPage, FlatValue},
    value_log::{PendingValue, ValuePlace},
    wal::{FlatChange, FlatRunWriter},
    FormatError,
};

use lignin_store::load::FRAME_FORMAT;

fn view(bytes: &[u8]) -> Result<FlatPage<'_>, FormatError> {
    FlatPage::from_validated(bytes, FRAME_FORMAT)
}

/// One record of a run, with its key as a range of the bytes that hold the
/// keys of the run.
enum Record<'a> {
    Remove(usize, usize, u16),
    Put(usize, usize, FlatValue<'a>, bool),
    Child(usize, usize, lignin_format::PageId),
}

/// The records of a run, before it is known whether an image is shorter.
#[derive(Default)]
struct Records<'a> {
    keys: Vec<u8>,
    removes: Vec<Record<'a>>,
    puts: Vec<Record<'a>>,
    len: usize,
}

impl<'a> Records<'a> {
    fn key(&mut self, key: FlatKey<'_>) -> (usize, usize) {
        let start = self.keys.len();
        key.append_to(&mut self.keys);
        (start, self.keys.len())
    }

    /// The entry under `key` leaves. An entry that leaves right after the
    /// one before it, with none between them that stays, is one more of
    /// the same range.
    fn remove(&mut self, key: FlatKey<'_>, follows: bool) {
        if follows
            && let Some(Record::Remove(_, _, count)) = self.removes.last_mut()
            && *count < u16::MAX
        {
            // A range is two bytes longer than a single remove.
            self.len += if *count == 1 { 2 } else { 0 };
            *count += 1;
            return;
        }
        let (start, end) = self.key(key);
        self.len += 3 + key.len();
        self.removes.push(Record::Remove(start, end, 1));
    }

    fn put(&mut self, key: FlatKey<'_>, value: FlatValue<'a>, replace: bool) {
        let (start, end) = self.key(key);
        self.len += 6 + key.len() + value.bytes().len();
        self.puts.push(Record::Put(start, end, value, replace));
    }

    fn child(&mut self, key: FlatKey<'_>, child: lignin_format::PageId) {
        let (start, end) = self.key(key);
        self.len += 7 + key.len();
        self.puts.push(Record::Child(start, end, child));
    }

    fn push(
        &self,
        prefix: Option<&[u8]>,
        run: &mut FlatRunWriter<'_>,
    ) -> Result<usize, FormatError> {
        // Removes come first: a range counts entries as the records before
        // it left them, and an insert would stand among them.
        for record in self.removes.iter().chain(&self.puts) {
            run.push(&match *record {
                Record::Remove(start, end, 1) => FlatChange::Remove {
                    key: &self.keys[start..end],
                },
                Record::Remove(start, end, count) => FlatChange::RemoveRange {
                    key: &self.keys[start..end],
                    count,
                },
                Record::Put(start, end, value, true) => FlatChange::Replace {
                    key: &self.keys[start..end],
                    value,
                },
                Record::Put(start, end, value, false) => FlatChange::Insert {
                    key: &self.keys[start..end],
                    value,
                },
                Record::Child(start, end, child) => FlatChange::InsertChild {
                    key: &self.keys[start..end],
                    child,
                },
            })?;
        }
        if let Some(prefix) = prefix {
            run.push(&FlatChange::SetPrefix { prefix })?;
        }
        Ok(self.removes.len() + self.puts.len() + usize::from(prefix.is_some()))
    }
}

/// Walks two lists of keys, both in key order, and tells `each` of every
/// key: the position it has in the old list, in the new one, or in both.
fn pair<'a>(
    old: impl Fn(usize) -> Option<FlatKey<'a>>,
    new: impl Fn(usize) -> Option<FlatKey<'a>>,
    mut each: impl FnMut(Option<usize>, Option<usize>),
) {
    let (mut before, mut after) = (0, 0);
    loop {
        let order = match (old(before), new(after)) {
            (None, None) => return,
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (Some(old), Some(new)) => old.cmp(&new),
        };
        each(
            (order != Ordering::Greater).then_some(before),
            (order != Ordering::Less).then_some(after),
        );
        before += usize::from(order != Ordering::Greater);
        after += usize::from(order != Ordering::Less);
    }
}

fn leaf_records<'a>(base: &FlatLeaf<'a>, page: &FlatLeaf<'a>) -> Records<'a> {
    let mut records = Records::default();
    let mut follows = false;
    pair(
        |slot| base.key(slot),
        |slot| page.key(slot),
        |old, new| {
            let (old, new) = (
                old.and_then(|slot| base.entry(slot)),
                new.and_then(|slot| page.entry(slot)),
            );
            match (old, new) {
                (Some((key, _)), None) => records.remove(key, follows),
                (None, Some((key, value))) => records.put(key, value, false),
                (Some((_, old)), Some((key, value))) if old != value => {
                    records.put(key, value, true);
                }
                _ => {}
            }
            // An entry that stays ends a range. One that arrives does not:
            // it is not there yet when the range is removed.
            follows = match (old, new) {
                (Some(_), None) => true,
                (Some(_), Some(_)) => false,
                _ => follows,
            };
        },
    );
    records
}

fn branch_records<'a>(base: &FlatBranch<'a>, page: &FlatBranch<'a>) -> Records<'a> {
    let mut records = Records::default();
    let mut follows = false;
    pair(
        |slot| base.separator(slot),
        |slot| page.separator(slot),
        |old, new| {
            let entry = |branch: &FlatBranch<'a>, slot: usize| {
                Some((branch.separator(slot)?, branch.child(slot + 1)?))
            };
            let (old, new) = (
                old.and_then(|slot| entry(base, slot)),
                new.and_then(|slot| entry(page, slot)),
            );
            // A separator that stays with another child leaves and comes
            // again: no record gives a separator another child.
            let stays = matches!((old, new), (Some((_, old)), Some((_, new))) if old == new);
            if !stays {
                if let Some((key, _)) = old {
                    records.remove(key, follows);
                }
                if let Some((key, child)) = new {
                    records.child(key, child);
                }
            }
            follows = match (old, stays) {
                (Some(_), false) => true,
                (Some(_), true) => false,
                (None, _) => follows,
            };
        },
    );
    records
}

/// Whether a commit has anything to write for a page whose image was
/// `base` and is `image`.
pub fn is_changed(base: Option<&[u8]>, image: &[u8]) -> Result<bool, FormatError> {
    let image = view(image)?.as_bytes();
    Ok(match base {
        Some(base) => view(base)?.as_bytes() != image,
        None => true,
    })
}

/// Writes into `run` the records that take a flat page from the image
/// `base` to the image `image`, and returns how many they are.
///
/// `base` is what the page was before the commit: `None` for a page the
/// commit made. The records are what the changes come to, however many
/// times the page was written in between: the entries that left, each key
/// or each range of them once, then those that arrived or have another
/// value, then the prefix if it is another one. The image itself is written
/// where there is no page to apply records to, where the records would be
/// longer than it, and where no record says the change: a page of another
/// kind or level, or a branch with another first child.
///
/// A page that did not change gives no record, and a run with none is not a
/// run: ask [`is_changed`] first.
pub fn push_changes(
    base: Option<&[u8]>,
    image: &[u8],
    run: &mut FlatRunWriter<'_>,
) -> Result<usize, FormatError> {
    let page = view(image)?;
    let image = page.as_bytes();
    let whole = |run: &mut FlatRunWriter<'_>| run.push(&FlatChange::Image(image)).map(|()| 1);
    let Some(base) = base else {
        return whole(run);
    };
    let records = match (view(base)?, page) {
        (FlatPage::Leaf(base), FlatPage::Leaf(page)) => leaf_records(&base, &page),
        (FlatPage::Branch(base), FlatPage::Branch(page))
            if base.level() == page.level() && base.child(0) == page.child(0) =>
        {
            branch_records(&base, &page)
        }
        _ => return whole(run),
    };
    let prefix = (view(base)?.prefix() != page.prefix()).then_some(page.prefix());
    let len = records.len + prefix.map_or(0, |prefix| 3 + prefix.len());
    if len >= 3 + image.len() {
        return whole(run);
    }
    records.push(prefix, run)
}

/// Gives the entry of the leaf `image` that holds the pending value `from`
/// the pending value `to` instead, where it stands: a commit puts the
/// place its group has for a new value into the page (`log-format.md`
/// LG21, LG22). Returns whether an entry held `from`.
///
/// The two are of one length, so nothing else in the image moves.
pub fn stamp_pending(
    image: &mut [u8],
    from: PendingValue,
    to: PendingValue,
) -> Result<bool, FormatError> {
    let (from, to) = (
        ValuePlace::Pending(from).encode(),
        ValuePlace::Pending(to).encode(),
    );
    let at = {
        let Some(leaf) = view(image)?.leaf() else {
            return Ok(false);
        };
        let cells =
            FlatLeaf::HEADER_LEN + leaf.prefix().len() + leaf.len() * FlatLeaf::ENTRY_OVERHEAD;
        let mut end = cells;
        leaf.iter().find_map(|(key, value)| {
            end += key.suffix().len() + value.bytes().len();
            (value == FlatValue::OutOfLine(&from)).then_some(end - from.len())
        })
    };
    let Some(place) = at.and_then(|at| image.get_mut(at..at + to.len())) else {
        return Ok(false);
    };
    place.copy_from_slice(&to);
    Ok(true)
}
