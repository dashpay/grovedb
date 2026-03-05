use grovedb_costs::{
    cost_return_on_error_into_no_add, CostContext, CostResult, CostsExt, OperationCost,
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

    /// Advances the iterator and returns the next key-element pair, or `None` if exhausted.
    pub fn next_element(
        &mut self,
        grove_version: &GroveVersion,
    ) -> CostResult<Option<KeyElementPair>, Error> {
        let mut cost = OperationCost::default();

        Ok(if self.raw_iter.valid().unwrap_add_cost(&mut cost) {
            if let Some((key, value)) = self
                .raw_iter
                .key()
                .unwrap_add_cost(&mut cost)
                .zip(self.raw_iter.value().unwrap_add_cost(&mut cost))
            {
                let element = cost_return_on_error_into_no_add!(
                    cost,
                    Element::raw_decode(value, grove_version)
                );
                let key_vec = key.to_vec();
                self.raw_iter.next().unwrap_add_cost(&mut cost);
                Some((key_vec, element))
            } else {
                None
            }
        } else {
            None
        })
        .wrap_with_cost(cost)
    }
}
