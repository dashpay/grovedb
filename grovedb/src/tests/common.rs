#[cfg(feature = "full")]
use merk::proofs::query::ProvedKeyValue;

#[cfg(feature = "full")]
use crate::{Element, Error};

#[cfg(feature = "full")]
pub fn compare_result_tuples(
    result_set: Vec<ProvedKeyValue>,
    expected_result_set: Vec<(Vec<u8>, Vec<u8>)>,
) {
    assert_eq!(expected_result_set.len(), result_set.len());
    for i in 0..expected_result_set.len() {
        assert_eq!(expected_result_set[i].0, result_set[i].key);
        assert_eq!(expected_result_set[i].1, result_set[i].value);
    }
}
#[cfg(feature = "full")]
fn deserialize_and_extract_item_bytes(raw_bytes: &[u8]) -> Result<Vec<u8>, Error> {
    let elem = Element::deserialize(raw_bytes)?;
    return match elem {
        Element::Item(item, _) => Ok(item),
        _ => Err(Error::CorruptedPath("expected only item type")),
    };
}

#[cfg(feature = "full")]
pub fn compare_result_sets(elements: &Vec<Vec<u8>>, result_set: &Vec<ProvedKeyValue>) {
    for i in 0..elements.len() {
        assert_eq!(
            deserialize_and_extract_item_bytes(&result_set[i].value).unwrap(),
            elements[i]
        )
    }
}
