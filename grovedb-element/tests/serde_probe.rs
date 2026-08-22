//! Regression probe for the `serde` feature forward: enabling only
//! `grovedb-element/serde` must give the re-exported `IndexAxis`
//! (defined in `grovedb-query`) its serde implementations, which
//! requires this crate's `serde` feature to forward to
//! `grovedb-query/serde`.
#![cfg(feature = "serde")]

#[test]
fn index_axis_has_serde_impls() {
    fn assert_serde<T: serde::Serialize + for<'de> serde::Deserialize<'de>>() {}
    assert_serde::<grovedb_element::indexed::IndexAxis>();
}
