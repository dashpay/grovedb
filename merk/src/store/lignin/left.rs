//! What a flat drop leaves behind (`flat-store.md`, rule 6).
//!
//! RocksDB removes the records of a subtree that was dropped flat when
//! the drop is drained, and not before: between the drop and its drain
//! they are records of their column family, and a cursor of another
//! subtree that steps from no key comes to them as to any record beside
//! its own ([`Place`](super::place::Place)). LigninDB drops the subtree
//! with its element. Whoever drops it states what the drop leaves
//! ([`LigninSettled::leave_behind`](super::LigninSettled::leave_behind)):
//! a **mark** for the prefix and the keyspace, which stands for one
//! record there, whatever the subtree held.
//!
//! A mark is a record of the aux keyspace of one subtree that the
//! handles are told ([`LigninNodeStore::left_behind_in`](super::LigninNodeStore::left_behind_in)),
//! under the tag of the keyspace it is of and then the 32 bytes of the
//! prefix ([`left_behind_key`]), with no value. So the marks of one
//! keyspace are in the order of their prefixes, and the nearest after a
//! prefix, or before it, is found by one seek.

use lignindb::txn::{Column, StandIns, Subtree, Unit};

use super::access::Prefix;
use crate::cost_model::Keyspace;

/// The tag that the key of a mark begins with: one for each keyspace
/// that has marks. The roots keyspace has none.
const fn tag(keyspace: Keyspace) -> Option<u8> {
    match keyspace {
        Keyspace::Data => Some(b'd'),
        Keyspace::Aux => Some(b'a'),
        Keyspace::Meta => Some(b'm'),
        Keyspace::Roots => None,
    }
}

/// The key of the mark that stands for what a flat drop left under
/// `prefix` in `keyspace`, in the aux keyspace of the subtree that keeps
/// the marks: the tag of the keyspace, and then the prefix. `None` for
/// the roots keyspace, which has no marks.
pub fn left_behind_key(keyspace: Keyspace, prefix: &[u8; 32]) -> Option<Vec<u8>> {
    tag(keyspace).map(|tag| [&[tag][..], prefix.as_slice()].concat())
}

/// What a cursor knows of the marks: where they are kept, and which
/// subtree and keyspace it is a cursor of.
#[derive(Clone, Copy, Debug)]
pub(super) struct Left {
    home: Prefix,
    own: Prefix,
    tag: u8,
}

impl Left {
    /// For a cursor of `keyspace` of the subtree `own`, whose handle was
    /// told that the marks are kept under `home`, if it was told.
    pub(super) fn of(home: Option<Prefix>, keyspace: Keyspace, own: &Prefix) -> Option<Self> {
        Some(Self {
            home: home?,
            own: *own,
            tag: tag(keyspace)?,
        })
    }

    /// The same for the subtree `own`.
    pub(super) fn of_subtree(self, own: &Prefix) -> Self {
        Self { own: *own, ..self }
    }

    /// Asks `ask` with the marks that stand for records after the
    /// cursor's subtree in the order of the prefixes, or with `back`
    /// before it: the marks of the cursor's keyspace under a greater
    /// prefix, or a smaller. The mark of the subtree's own prefix is
    /// none of them.
    pub(super) fn beside<T>(&self, back: bool, ask: impl FnOnce(StandIns<'_>) -> T) -> T {
        let own = [&[self.tag][..], self.own.as_slice()].concat();
        let (low, high) = match back {
            true => (vec![self.tag], own),
            // The first key after the subtree's own mark.
            false => ([own.as_slice(), &[0]].concat(), vec![self.tag + 1]),
        };
        ask(StandIns {
            unit: Unit::new(Column::Aux, Subtree::new(self.home)),
            low: &low,
            high: &high,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The marks of a keyspace are in the order of their prefixes, apart
    /// from those of the other keyspaces, and the two ranges of a cursor
    /// hold every mark of its keyspace but its own.
    #[test]
    fn the_marks_beside_a_prefix_are_those_of_its_keyspace_under_another_prefix() {
        let prefix = |byte: u8| [byte; 32];
        let own = prefix(5);
        let marks: Vec<(Keyspace, [u8; 32])> = [Keyspace::Data, Keyspace::Aux, Keyspace::Meta]
            .into_iter()
            .flat_map(|keyspace| [0, 4, 5, 6, 0xFF].map(|byte| (keyspace, prefix(byte))))
            .collect();
        assert_eq!(left_behind_key(Keyspace::Roots, &own), None);
        assert!(Left::of(Some(prefix(9)), Keyspace::Roots, &own).is_none());
        assert!(Left::of(None, Keyspace::Data, &own).is_none());
        for keyspace in [Keyspace::Data, Keyspace::Aux, Keyspace::Meta] {
            let left = Left::of(Some(prefix(9)), keyspace, &prefix(1)).unwrap();
            let left = left.of_subtree(&own);
            for back in [false, true] {
                let within: Vec<(Keyspace, [u8; 32])> = left.beside(back, |stand_ins| {
                    assert_eq!(
                        stand_ins.unit,
                        Unit::new(Column::Aux, Subtree::new(prefix(9)))
                    );
                    let held = |mark: &&(Keyspace, [u8; 32])| {
                        let key = left_behind_key(mark.0, &mark.1).unwrap();
                        assert_eq!(key.len(), 33);
                        stand_ins.low <= key.as_slice() && key.as_slice() < stand_ins.high
                    };
                    marks.iter().filter(held).copied().collect()
                });
                let expected: Vec<(Keyspace, [u8; 32])> = (marks.iter())
                    .filter(|(of, mark)| {
                        *of == keyspace
                            && match back {
                                false => *mark > own,
                                true => *mark < own,
                            }
                    })
                    .copied()
                    .collect();
                assert_eq!(within, expected, "{keyspace:?}, back {back}");
                assert_eq!(within.len(), 2);
            }
        }
    }
}
