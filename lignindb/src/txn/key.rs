//! Record keys, the units they belong to, and the key set of a commit.

use std::{
    collections::{BTreeSet, HashMap},
    fmt,
    ops::Bound,
};

/// One of the four columns a record lives in (`transactions.md` ST1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Column {
    /// Merk nodes, or the flat entries of an append-only tree.
    Data,
    /// Auxiliary records.
    Aux,
    /// The base root key.
    Roots,
    /// Merk metadata, and the pending-drop records.
    Meta,
}

impl Column {
    /// Every column.
    pub const ALL: [Self; 4] = [Self::Data, Self::Aux, Self::Roots, Self::Meta];
}

impl fmt::Display for Column {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Data => "data",
            Self::Aux => "aux",
            Self::Roots => "roots",
            Self::Meta => "meta",
        })
    }
}

/// The 32 bytes that name a subtree: the prefix of its path, or a prefix
/// derived from one (ST1).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Subtree([u8; Self::LEN]);

impl Subtree {
    /// The length of a subtree's name in bytes.
    pub const LEN: usize = 32;

    /// The subtree with this prefix.
    pub const fn new(prefix: [u8; Self::LEN]) -> Self {
        Self(prefix)
    }

    /// The prefix.
    pub const fn as_bytes(&self) -> &[u8; Self::LEN] {
        &self.0
    }
}

impl fmt::Debug for Subtree {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Subtree({self})")
    }
}

impl fmt::Display for Subtree {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in &self.0[..4] {
            write!(f, "{byte:02x}")?;
        }
        f.write_str("..")
    }
}

/// A column of a subtree: the set of record keys that share both. It is the
/// unit a write scope claims (SC2), and the keyspace a cursor walks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Unit {
    /// The column.
    pub column: Column,
    /// The subtree.
    pub subtree: Subtree,
}

impl Unit {
    /// The unit of `column` under `subtree`.
    pub const fn new(column: Column, subtree: Subtree) -> Self {
        Self { column, subtree }
    }

    /// The Merk nodes, or the flat entries, of `subtree`.
    pub const fn data(subtree: Subtree) -> Self {
        Self::new(Column::Data, subtree)
    }
}

impl fmt::Display for Unit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} of {}", self.column, self.subtree)
    }
}

/// Whether `key` lies in the interval from `start` to before `end`, or to
/// the end of the keyspace with no `end`.
pub(crate) fn in_range(key: &[u8], start: &[u8], end: Option<&[u8]>) -> bool {
    key >= start && end.is_none_or(|end| key < end)
}

/// Whether two intervals share a key. An interval contains a key whether or
/// not a record exists under it (ST4), so two intervals meet when their
/// bounds overlap.
pub(crate) fn ranges_meet(a: (&[u8], Option<&[u8]>), b: (&[u8], Option<&[u8]>)) -> bool {
    let below = |start: &[u8], end: Option<&[u8]>| end.is_none_or(|end| start < end);
    below(a.0, a.1) && below(b.0, b.1) && below(a.0, b.1) && below(b.0, a.1)
}

/// The keys and intervals one commit wrote in one unit.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct UnitKeys {
    pub(crate) keys: BTreeSet<Box<[u8]>>,
    pub(crate) ranges: Vec<(Box<[u8]>, Option<Box<[u8]>>)>,
}

impl UnitKeys {
    /// The keys of the set that lie in the interval.
    pub(crate) fn keys_in<'a>(
        &'a self,
        start: &'a [u8],
        end: Option<&'a [u8]>,
    ) -> impl Iterator<Item = &'a [u8]> + 'a {
        let upper = end.map_or(Bound::Unbounded, Bound::Excluded);
        // An interval that ends at or before its start holds no key, and is
        // not one a map can be asked for.
        let inside = (!end.is_some_and(|end| end <= start))
            .then(|| self.keys.range::<[u8], _>((Bound::Included(start), upper)));
        inside.into_iter().flatten().map(|key| &**key)
    }
}

/// The key set of a commit (`transactions.md` CF1): the record keys its
/// writes put or delete and the ranges its range deletes cover, each
/// counted once.
///
/// It is what the conflict rule compares with the write set of every other
/// open write transaction (CF2, CF3), and what tells a transaction whether
/// a commit that landed while it was open wrote a key it had written (the
/// overlap set of WT7).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KeySet {
    pub(crate) units: HashMap<Unit, UnitKeys>,
    pub(crate) subtrees: BTreeSet<Subtree>,
    pub(crate) everything: bool,
}

impl KeySet {
    /// A key set with no key.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds the record key `key` of `unit`: a put or a delete of it.
    pub fn key(&mut self, unit: Unit, key: &[u8]) -> &mut Self {
        self.units.entry(unit).or_default().keys.insert(key.into());
        self
    }

    /// Adds the keys of `unit` from `start` to before `end`, or to the end
    /// of the unit with no `end`: a range delete, or a bulk load counted as
    /// one write of its range (CF5).
    pub fn range(&mut self, unit: Unit, start: &[u8], end: Option<&[u8]>) -> &mut Self {
        let range = (start.into(), end.map(Into::into));
        let ranges = &mut self.units.entry(unit).or_default().ranges;
        if !ranges.contains(&range) {
            ranges.push(range);
        }
        self
    }

    /// Adds every key of every column under `subtree`: a subtree drop
    /// (CV9).
    pub fn subtree(&mut self, subtree: Subtree) -> &mut Self {
        self.subtrees.insert(subtree);
        self
    }

    /// Adds every key of every column: a wipe (CV3).
    pub fn everything(&mut self) -> &mut Self {
        self.everything = true;
        self
    }

    /// Whether the set has no key and no range.
    pub fn is_empty(&self) -> bool {
        !self.everything
            && self.subtrees.is_empty()
            && self
                .units
                .values()
                .all(|unit| unit.keys.is_empty() && unit.ranges.is_empty())
    }

    /// Whether the set holds the record key `key` of `unit`, as a key or
    /// inside a range.
    pub fn meets_key(&self, unit: Unit, key: &[u8]) -> bool {
        self.covers_unit(unit)
            || self.units.get(&unit).is_some_and(|keys| {
                keys.keys.contains(key)
                    || keys
                        .ranges
                        .iter()
                        .any(|(start, end)| in_range(key, start, end.as_deref()))
            })
    }

    /// Whether the set shares a key with the interval of `unit` from
    /// `start` to before `end`.
    pub fn meets_range(&self, unit: Unit, start: &[u8], end: Option<&[u8]>) -> bool {
        if end.is_some_and(|end| end <= start) {
            return false;
        }
        self.covers_unit(unit)
            || self.units.get(&unit).is_some_and(|keys| {
                keys.keys_in(start, end).next().is_some()
                    || keys
                        .ranges
                        .iter()
                        .any(|(from, to)| ranges_meet((from, to.as_deref()), (start, end)))
            })
    }

    /// Whether the set holds every key of `unit`.
    pub(crate) fn covers_unit(&self, unit: Unit) -> bool {
        self.everything || self.subtrees.contains(&unit.subtree)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit(column: Column, name: u8) -> Unit {
        Unit::new(column, Subtree::new([name; 32]))
    }

    #[test]
    fn a_key_meets_itself_and_the_ranges_that_hold_it() {
        let aux = unit(Column::Aux, 1);
        let mut set = KeySet::new();
        assert!(set.is_empty());
        set.key(aux, b"k").key(aux, b"k");
        assert!(!set.is_empty());
        assert!(set.meets_key(aux, b"k"));
        assert!(!set.meets_key(aux, b"j"));
        // The column and the subtree are parts of the key (case A6).
        assert!(!set.meets_key(unit(Column::Data, 1), b"k"));
        assert!(!set.meets_key(unit(Column::Aux, 2), b"k"));
        assert!(set.meets_range(aux, b"a", Some(b"z")));
        assert!(set.meets_range(aux, b"k", None));
        assert!(!set.meets_range(aux, b"l", None));
        assert!(!set.meets_range(aux, b"a", Some(b"k")));
        // An interval that ends where it starts holds no key.
        assert!(!set.meets_range(aux, b"k", Some(b"k")));
        assert!(!set.meets_range(aux, b"z", Some(b"a")));
        assert_eq!(set.units[&aux].keys_in(b"z", Some(b"a")).count(), 0);
        assert_eq!(set.units[&aux].keys_in(b"a", Some(b"z")).count(), 1);
    }

    #[test]
    fn a_range_meets_the_keys_and_the_ranges_inside_it() {
        let data = unit(Column::Data, 7);
        let mut set = KeySet::new();
        set.range(data, b"c", Some(b"f"))
            .range(data, b"c", Some(b"f"));
        assert_eq!(set.units[&data].ranges.len(), 1);
        assert!(set.meets_key(data, b"c"));
        assert!(set.meets_key(data, b"eee"));
        assert!(!set.meets_key(data, b"f"));
        assert!(!set.meets_key(data, b"b"));
        assert!(set.meets_range(data, b"a", Some(b"d")));
        assert!(set.meets_range(data, b"e", None));
        assert!(!set.meets_range(data, b"f", None));
        assert!(!set.meets_range(data, b"a", Some(b"c")));
        set.range(data, b"x", None);
        assert!(set.meets_key(data, b"zzz"));
        assert!(set.meets_range(data, b"y", Some(b"z")));
    }

    #[test]
    fn a_dropped_subtree_and_a_wipe_meet_whole_units() {
        let mut set = KeySet::new();
        set.subtree(Subtree::new([3; 32]));
        assert!(!set.is_empty());
        for column in Column::ALL {
            assert!(set.meets_key(unit(column, 3), b"any"));
            assert!(set.meets_range(unit(column, 3), b"", None));
            assert!(!set.meets_key(unit(column, 4), b"any"));
        }
        let mut wipe = KeySet::new();
        wipe.everything();
        assert!(!wipe.is_empty());
        assert!(wipe.meets_key(unit(Column::Meta, 9), b""));
        assert!(wipe.meets_range(unit(Column::Roots, 9), b"a", Some(b"b")));
    }

    #[test]
    fn names_are_shown() {
        let unit = unit(Column::Roots, 0xab);
        assert_eq!(unit.to_string(), "roots of abababab..");
        assert_eq!(format!("{:?}", unit.subtree), "Subtree(abababab..)");
        assert_eq!(unit.subtree.as_bytes(), &[0xab; 32]);
        let names: Vec<String> = Column::ALL.iter().map(Column::to_string).collect();
        assert_eq!(names, ["data", "aux", "roots", "meta"]);
        assert_eq!(Unit::data(unit.subtree).column, Column::Data);
    }
}
