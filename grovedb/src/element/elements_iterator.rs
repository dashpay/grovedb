use grovedb_costs::{
    cost_return_on_error_into_no_add, cost_return_on_error_no_add, CostContext, CostResult,
    CostsExt, OperationCost,
};
use grovedb_element::Element;
use grovedb_merk::element::decode::ElementDecodeExtensions;
use grovedb_storage::RawIterator;
use grovedb_version::version::GroveVersion;

use crate::{query_result_type::KeyElementPair, Error};

/// Extension trait providing an iterator constructor on `Element`.
pub trait ElementIteratorExtensions {
    /// Creates a new `ElementsIterator` positioned at the first entry.
    fn iterator<I: RawIterator>(raw_iter: I) -> CostContext<ElementsIterator<I>>;
}

impl ElementIteratorExtensions for Element {
    /// Iterator
    fn iterator<I: RawIterator>(mut raw_iter: I) -> CostContext<ElementsIterator<I>> {
        let mut cost = OperationCost::default();
        raw_iter.seek_to_first().unwrap_add_cost(&mut cost);
        ElementsIterator::new(raw_iter).wrap_with_cost(cost)
    }
}

/// Why a cursor that stands on a key shows no value under it: the error of
/// its storage where the read of the record failed
/// ([`RawIterator::status`]), and otherwise a record that is not there
/// where the cursor says one is.
pub(crate) fn no_value_under_cursor<I: RawIterator>(raw_iter: &I) -> Error {
    match raw_iter.status() {
        Err(error) => Error::StorageError(error),
        Ok(()) => Error::CorruptedData("expected iterator value but got None".to_string()),
    }
}

/// An iterator over stored elements, wrapping a low-level storage iterator.
pub struct ElementsIterator<I: RawIterator> {
    /// The underlying raw storage iterator.
    raw_iter: I,
}

impl<I: RawIterator> ElementsIterator<I> {
    /// Creates a new `ElementsIterator` from the given raw iterator.
    pub fn new(raw_iter: I) -> Self {
        ElementsIterator { raw_iter }
    }

    /// Advances the iterator and returns the next key-element pair, or
    /// `None` at the end of the records.
    ///
    /// A walk that the storage could not go on with is no walk that came to
    /// its end: where the cursor stands on no record, or shows none under
    /// its key, because a move or a read of it failed, this is the storage
    /// error ([`RawIterator::status`]). A key that the cursor shows with no
    /// record under it and no error is one whose record was removed under
    /// the cursor, and the walk goes on behind it.
    pub fn next_element(
        &mut self,
        grove_version: &GroveVersion,
    ) -> CostResult<Option<KeyElementPair>, Error> {
        let mut cost = OperationCost::default();

        let next = loop {
            if !self.raw_iter.valid().unwrap_add_cost(&mut cost) {
                break None;
            }
            let key = self.raw_iter.key().unwrap_add_cost(&mut cost);
            let key = key.map(<[u8]>::to_vec);
            let value = self.raw_iter.value().unwrap_add_cost(&mut cost);
            match (key, value) {
                (Some(key), Some(value)) => {
                    let element = cost_return_on_error_into_no_add!(
                        cost,
                        Element::raw_decode(value, grove_version)
                    );
                    self.raw_iter.next().unwrap_add_cost(&mut cost);
                    break Some((key, element));
                }
                (Some(_), None) if self.raw_iter.status().is_ok() => {
                    self.raw_iter.next().unwrap_add_cost(&mut cost);
                }
                _ => break None,
            }
        };
        if next.is_none() {
            cost_return_on_error_no_add!(cost, self.raw_iter.status().map_err(Error::StorageError));
        }
        Ok(next).wrap_with_cost(cost)
    }
}

#[cfg(test)]
mod tests {
    use grovedb_merk::{tree::TreeNode, TreeFeatureType::BasicMerkNode};
    use grovedb_storage::Error as StorageError;

    use super::*;

    /// A cursor over records of the test's own: a key can have no record
    /// under it, and the cursor can say that it failed.
    struct Records {
        records: Vec<(Vec<u8>, Option<Vec<u8>>)>,
        at: usize,
        failed: bool,
    }

    fn free<T>(value: T) -> CostContext<T> {
        value.wrap_with_cost(OperationCost::default())
    }

    impl RawIterator for Records {
        fn seek_to_first(&mut self) -> CostContext<()> {
            self.at = 0;
            free(())
        }

        fn seek_to_last(&mut self) -> CostContext<()> {
            self.at = self.records.len().saturating_sub(1);
            free(())
        }

        fn seek<K: AsRef<[u8]>>(&mut self, key: K) -> CostContext<()> {
            let found = self
                .records
                .iter()
                .position(|(at, _)| &at[..] >= key.as_ref());
            self.at = found.unwrap_or(self.records.len());
            free(())
        }

        fn seek_for_prev<K: AsRef<[u8]>>(&mut self, key: K) -> CostContext<()> {
            let found = self
                .records
                .iter()
                .rposition(|(at, _)| &at[..] <= key.as_ref());
            self.at = found.unwrap_or(self.records.len());
            free(())
        }

        fn next(&mut self) -> CostContext<()> {
            self.at += 1;
            free(())
        }

        fn prev(&mut self) -> CostContext<()> {
            self.at = self.at.checked_sub(1).unwrap_or(self.records.len());
            free(())
        }

        fn value(&self) -> CostContext<Option<&[u8]>> {
            free(
                self.records
                    .get(self.at)
                    .and_then(|(_, value)| value.as_deref()),
            )
        }

        fn key(&self) -> CostContext<Option<&[u8]>> {
            free(self.records.get(self.at).map(|(key, _)| key.as_slice()))
        }

        fn valid(&self) -> CostContext<bool> {
            free(self.at < self.records.len())
        }

        fn status(&self) -> Result<(), StorageError> {
            match self.failed {
                true => Err(StorageError::StorageError("the walk failed".to_owned())),
                false => Ok(()),
            }
        }
    }

    /// A cursor over an item under each of `keys`, or under a key with
    /// `false` no record.
    fn records(keys: &[(&[u8], bool)], failed: bool) -> Records {
        let version = GroveVersion::latest();
        let records = keys.iter().map(|(key, there)| {
            let item = Element::new_item(key.to_vec());
            let value = item.serialize(version).expect("an element");
            let node = TreeNode::new(key.to_vec(), value, None, BasicMerkNode).unwrap();
            (key.to_vec(), there.then_some(node.encode()))
        });
        Records {
            records: records.collect(),
            at: 7,
            failed,
        }
    }

    /// The keys a walk gives, and how it ended.
    fn walked(mut walk: ElementsIterator<Records>) -> (Vec<Vec<u8>>, Result<(), Error>) {
        let version = GroveVersion::latest();
        let mut keys = Vec::new();
        loop {
            match walk.next_element(version).unwrap() {
                Ok(Some((key, element))) => {
                    assert_eq!(element, Element::new_item(key.clone()));
                    keys.push(key);
                }
                Ok(None) => return (keys, Ok(())),
                Err(error) => return (keys, Err(error)),
            }
        }
    }

    #[test]
    fn a_key_with_no_record_is_passed_over_and_a_walk_that_failed_has_not_ended() {
        let all: [(&[u8], bool); 4] = [(b"a", true), (b"b", false), (b"c", true), (b"d", false)];
        // Records that were removed under the cursor: the walk gives the
        // others and comes to its end.
        let walk = Element::iterator(records(&all, false)).unwrap();
        let (keys, ended) = walked(walk);
        assert_eq!(keys, [b"a".to_vec(), b"c".to_vec()]);
        assert!(ended.is_ok(), "{ended:?}");

        // A record that could not be read: the walk gives the records
        // before it and then the storage's error. So does a walk whose
        // cursor failed where it stands on no record.
        for keys_of_walk in [&all[..], &all[..1]] {
            let walk = Element::iterator(records(keys_of_walk, true)).unwrap();
            let (keys, ended) = walked(walk);
            assert_eq!(keys, [b"a".to_vec()]);
            let error = ended.expect_err("a walk that failed");
            assert!(matches!(error, Error::StorageError(_)), "{error}");
        }
        let (keys, ended) = walked(Element::iterator(records(&[], true)).unwrap());
        assert!(keys.is_empty());
        assert!(matches!(ended, Err(Error::StorageError(_))), "{ended:?}");
    }

    #[test]
    fn what_a_cursor_does_not_show_is_the_storages_error_where_it_has_one() {
        let mut cursor = records(&[(b"a", true), (b"c", false)], true);
        let error = no_value_under_cursor(&cursor);
        assert!(matches!(error, Error::StorageError(_)), "{error}");
        cursor.failed = false;
        let error = no_value_under_cursor(&cursor);
        assert!(
            matches!(&error, Error::CorruptedData(what) if what.contains("iterator value")),
            "{error}"
        );
        // The cursor of these tests moves as a cursor does.
        cursor.seek(b"b").unwrap();
        assert_eq!(cursor.key().unwrap(), Some(b"c".as_slice()));
        assert_eq!(cursor.value().unwrap(), None);
        cursor.prev().unwrap();
        assert_eq!(cursor.key().unwrap(), Some(b"a".as_slice()));
        cursor.prev().unwrap();
        assert!(!cursor.valid().unwrap());
        cursor.seek_for_prev(b"b").unwrap();
        assert_eq!(cursor.key().unwrap(), Some(b"a".as_slice()));
        cursor.seek_to_last().unwrap();
        assert_eq!(cursor.key().unwrap(), Some(b"c".as_slice()));
    }
}
